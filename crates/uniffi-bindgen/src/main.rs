//! UniFFI's Swift bindings generator (`uniffi-bindgen-swift`), built from
//! the workspace's own UniFFI so the bindings always match `crates/ffi`.
//! `scripts/apple/build-core.sh` runs it on the static library.

fn main() {
    uniffi::uniffi_bindgen_swift()
}
