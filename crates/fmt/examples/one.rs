//! Format one file to stdout (for debugging the formatter).
//!
//!     cargo run -p neoscad-fmt --example one -- FILE [WIDTH]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let text = std::fs::read(&args[0]).unwrap();
    let mut cfg = scadfmt::Config::default();
    if let Some(w) = args.get(1) {
        cfg.width = w.parse().unwrap();
    }
    match scadfmt::format(&text, &cfg) {
        Ok(out) => print!("{}", String::from_utf8_lossy(&out)),
        Err(e) => eprintln!("{e}"),
    }
}
