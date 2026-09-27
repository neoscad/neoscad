//! `neoscad lsp --stdio`: the language server (`crates/lsp`) for editors
//! other than the app (VS Code, Zed, Neovim, Helix ...), over stdio with
//! the protocol's framing: each message a `Content-Length` header, a
//! blank line, and the JSON.
//!
//! The server core has no clock and no threads; this host adds both. One
//! thread reads messages and answers them in order. Diagnostics are
//! debounced here: after a change, once the messages pause for
//! [`DEBOUNCE`], a second thread evaluates the changed documents
//! (`Server::publish_diagnostics`) while the first keeps answering
//! completion and hover. A change made during an evaluation stops it.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use clap::Parser;

/// How long the messages must pause before diagnostics are computed. An
/// editor sends a change per keystroke (CodeMirror's client batches them
/// itself), and each evaluation of a large model can take a while.
const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Parser, Debug)]
#[command(
    name = "neoscad lsp",
    about = "The OpenSCAD language server for editors (Language Server Protocol)"
)]
struct Args {
    /// Talk over stdin and stdout (the only transport, and the default).
    #[arg(long)]
    stdio: bool,

    /// Change a resource limit of the diagnostics' evaluations,
    /// NAME=VALUE (repeatable; 'off' for none), as `neoscad serve`.
    #[arg(long = "limit", value_name = "NAME=VALUE", action = clap::ArgAction::Append)]
    limit: Vec<String>,

    /// Append every message received and sent to FILE.
    #[arg(long = "log", value_name = "FILE")]
    log: Option<PathBuf>,
}

pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad lsp")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 1 } else { 0 };
        }
    };
    let _ = a.stdio;
    let host = crate::host::Host::from_env();
    let mut cfg = host.session_config(crate::host::entropy_seed());
    // An editor runs whatever is being typed: the agent limits apply
    // unless the user changes them, as for `serve`.
    cfg.limits = match crate::limits::from_flags(session::Limits::AGENT, &a.limit) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("neoscad lsp: {e}");
            return 1;
        }
    };
    let log = a.log.and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    let host = Arc::new(Host {
        session: session::Session::new(cfg),
        server: lsp::Server::new(lsp::Options {
            sync_session: true,
            limits: None,
            host_diagnostics: false,
        }),
        out: Mutex::new(Box::new(std::io::stdout())),
        log: log.map(Mutex::new),
    });
    // Parsing recurses over the program, as evaluation does: the same
    // stack as the command line's.
    let code = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        serve(&host, BufReader::new(std::io::stdin()))
    });
    u8::try_from(code).unwrap_or(1)
}

struct Host {
    session: session::Session,
    server: lsp::Server,
    out: Mutex<Box<dyn Write + Send>>,
    log: Option<Mutex<std::fs::File>>,
}

impl Host {
    fn trace(&self, dir: &str, msg: &str) {
        if let Some(f) = &self.log {
            let mut f = f.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = writeln!(f, "{dir} {msg}");
        }
    }

    fn send(&self, msg: &str) {
        self.trace("->", msg);
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = write!(out, "Content-Length: {}\r\n\r\n{msg}", msg.len());
        let _ = out.flush();
    }
}

/// One framed message, `None` at the end of input. Headers other than
/// `Content-Length` (`Content-Type`) are read and ignored.
pub fn read_message(r: &mut impl BufRead) -> Option<String> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let mut body = vec![0u8; len?];
    r.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// Answer messages until `exit` (or the end of input); the exit code.
/// The reader thread is not joined: it may be blocked on input the
/// client never closes, and the process ends with this function anyway.
fn serve(host: &Arc<Host>, mut input: impl BufRead + Send + 'static) -> i32 {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let (diag_tx, diag_rx) = mpsc::channel::<()>();
    let publisher = {
        let host = host.clone();
        std::thread::Builder::new()
            .name("diagnostics".into())
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .spawn(move || {
                while diag_rx.recv().is_ok() {
                    while diag_rx.try_recv().is_ok() {}
                    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        host.server.publish_diagnostics(&host.session)
                    }))
                    .unwrap_or_default();
                    for m in out {
                        host.send(&m);
                    }
                }
            })
    };
    if publisher.is_err() {
        eprintln!("neoscad lsp: cannot start the diagnostics thread");
        return 1;
    }
    std::thread::spawn(move || {
        loop {
            let m = read_message(&mut input);
            let end = m.is_none();
            if tx.send(m).is_err() || end {
                break;
            }
        }
    });
    let mut due: Option<Instant> = None;
    loop {
        let next = match due {
            Some(t) => rx.recv_timeout(t.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Some(m)) => {
                host.trace("<-", &m);
                for out in host.server.handle(&host.session, &m) {
                    host.send(&out);
                }
                if host.server.exited() {
                    return host.server.exit_code();
                }
                if host.server.diagnostics_pending() {
                    due = Some(Instant::now() + DEBOUNCE);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                due = None;
                let _ = diag_tx.send(());
            }
            // The client went away without `exit`.
            Ok(None) | Err(RecvTimeoutError::Disconnected) => return 1,
        }
    }
}
