//! Links the wasm32 module with the stack neoscad's evaluator expects
//! (`eval::recursion::WASM_STACK_SIZE`; rustc's default is 1 MiB), as the
//! future web build must too. A test checks that the two numbers agree.

/// Must equal `eval::recursion::WASM_STACK_SIZE`.
const STACK_SIZE: usize = 8 << 20;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-env=WASM_CHECK_STACK_SIZE={STACK_SIZE}");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo::rustc-link-arg-cdylib=-zstack-size={STACK_SIZE}");
    }
}
