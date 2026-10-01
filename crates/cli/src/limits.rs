//! Resource limits as the command line, `neoscad serve` and `neoscad mcp`
//! take them (`eval::limits`).
//!
//! - `serve` and `mcp` start from [`Limits::AGENT`]: they run models an
//!   agent or an editor wrote, and one runaway `$fn` must not take the
//!   machine down. `--limit NAME=VALUE` changes one (`off` removes it).
//! - The OpenSCAD-compatible command line starts unlimited, as OpenSCAD
//!   is; `--limit` sets limits for that run (which then stays in-process).
//! - The memory limit is measured as well as estimated where the platform
//!   allows (`crate::memory`): a run stops with a "(measured)" error once
//!   the process uses more than the limit, and in `serve` and `mcp` the
//!   limit is a budget the whole server shares, cached geometry being
//!   evicted before a request fails (`session::memory`).
//! - A served request may carry a `limits` object. The command line never
//!   sends one on its `cli.*` requests (a run with `--limit` is not
//!   delegated), and the server fills in an explicit unlimited one
//!   (`serve::cli`), so a delegated run is unlimited as it would be in
//!   the process. Leaving `limits` absent is not enough: the session
//!   would then apply the server's own `Limits::AGENT`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

pub use eval::limits::{Guard, Limit, Limits};

/// `base` with each `--limit NAME=VALUE` applied, or the message for the
/// first bad one.
pub fn from_flags(base: Limits, flags: &[String]) -> Result<Limits, String> {
    let mut l = base;
    for f in flags {
        l.apply_flag(f)?;
    }
    Ok(l)
}

/// Milliseconds since this clock was made: what the guard's deadline is
/// measured on.
pub fn clock() -> eval::limits::Clock {
    let t0 = std::time::Instant::now();
    Arc::new(move || t0.elapsed().as_secs_f64() * 1000.0)
}

/// A guard for a one-shot run (`None` when unlimited), with the interrupt
/// flag it stops the run through.
pub fn guard(limits: Limits) -> Option<(Arc<Guard>, Arc<AtomicBool>)> {
    if limits.is_none() {
        return None;
    }
    let flag = Arc::new(AtomicBool::new(false));
    let clock = clock();
    // The memory limit measured too (`crate::memory`), read at most every
    // 10 ms: a one-shot run has no caches worth evicting first.
    let probe = crate::memory::probe().map(|p| session::memory::throttled(p, clock.clone()));
    Some((
        Arc::new(Guard::new(limits, flag.clone(), Some(clock)).with_probe(probe)),
        flag,
    ))
}

/// The limits as JSON: each key in its unit (`time` seconds, `memory`
/// MiB), `null` for none.
pub fn json(l: &Limits) -> Value {
    let mut o = serde_json::Map::new();
    for k in Limit::ALL {
        o.insert(k.key().into(), json!(l.get(k)));
    }
    Value::Object(o)
}

/// A request's `limits` parameter on top of `base`: an object of limit
/// names to numbers (in [`json`]'s units) or `null`/`"off"` for none.
/// `None` when the request has none.
pub fn of_params(params: &Value, base: Limits) -> Result<Option<Limits>, String> {
    let o = match params.get("limits") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(o)) => o,
        Some(_) => return Err("\"limits\" must be an object, e.g. {\"fragments\": 20000}".into()),
    };
    let mut l = base;
    for (k, v) in o {
        let limit = Limit::from_key(k).ok_or_else(|| {
            let keys: Vec<&str> = Limit::ALL.iter().map(|l| l.key()).collect();
            format!("unknown limit \"{k}\" (the limits are {})", keys.join(", "))
        })?;
        match v {
            Value::Null => l.put(limit, None),
            Value::String(s) => l.set(limit, s)?,
            Value::Number(n) => l.set(limit, &n.to_string())?,
            _ => return Err(format!("limit \"{k}\" must be a number or null")),
        }
    }
    Ok(Some(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_override_the_base() {
        let p = json!({"limits": {"fragments": 20000, "time": null, "memory": "1G"}});
        let l = of_params(&p, Limits::AGENT).unwrap().unwrap();
        assert_eq!(l.fragments, Some(20_000));
        assert_eq!(l.time, None);
        assert_eq!(l.memory, Some(1 << 30));
        assert_eq!(l.slices, Limits::AGENT.slices);
        assert!(of_params(&json!({}), Limits::AGENT).unwrap().is_none());
        assert!(of_params(&json!({"limits": {"frags": 1}}), Limits::AGENT).is_err());
        assert_eq!(json(&Limits::AGENT)["memory"], json!(4096.0));
    }
}
