//! `neoscad-gtk`: the Linux desktop app (docs/linux-app.md). Built only
//! with the `gtk` feature (`Cargo.toml` says why).

// The global allocator, as in `neoscad` and the macOS app's core
// (docs/architecture.md, "Stack").
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod app;

fn main() -> gtk::glib::ExitCode {
    app::run()
}
