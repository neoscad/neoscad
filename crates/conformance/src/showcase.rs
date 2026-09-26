//! The fixed showcase set (`conformance/showcase.json`, from
//! docs/audits/phase0.md part C). Nothing is rendered yet; this only checks
//! that every listed input and expected image exists in the reference
//! checkout, so a reference update that moves a file is caught early.

use serde::Deserialize;

use crate::ctx::Ctx;

#[derive(Debug, Deserialize)]
pub struct Showcase {
    pub models: Vec<Model>,
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub id: String,
    pub input: String,
    pub expected: String,
}

pub fn load(ctx: &Ctx) -> Result<Showcase, String> {
    let path = ctx.showcase_path();
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Returns the number of missing files (0 = valid).
pub fn check(ctx: &Ctx) -> Result<usize, String> {
    let s = load(ctx)?;
    let mut missing = 0;
    let mut ids = std::collections::BTreeSet::new();
    for m in &s.models {
        if !ids.insert(m.id.as_str()) {
            println!("duplicate id {}", m.id);
            missing += 1;
        }
        for p in [&m.input, &m.expected] {
            if !ctx.ref_root.join(p).is_file() {
                println!("missing: {p} ({})", m.id);
                missing += 1;
            }
        }
    }
    println!("showcase: {} models, {} problems", s.models.len(), missing);
    Ok(missing)
}
