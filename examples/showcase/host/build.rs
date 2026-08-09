//! Embeds the rpath the Flutter engine needs, so `cargo run -p duet-showcase`
//! works with nothing set in the environment.
//!
//! # Why an embedder has to do this at all
//!
//! `crates/duet-backend-macos/build.rs` already emits
//! `-Wl,-rpath,<FlutterMacOS.framework's directory>` — but a `cargo:rustc-link-arg`
//! applies to the package that emitted it and to *its* binaries, examples and
//! tests. It does not reach a downstream crate's binary. So a real application
//! that depends on `duet-backend-macos` links fine and then dies at startup with
//! `Library not loaded: @rpath/FlutterMacOS.framework/...`, which is exactly what
//! this showcase did on its first run.
//!
//! Repeating it here is the workaround, not the design. `duet-backend-macos`
//! could propagate this by declaring `links = "FlutterMacOS"` and emitting the
//! directory as build metadata for dependents to read; that is a change to a
//! library this example is not allowed to make, so it is written up in
//! `examples/showcase/README.md` under "What the library could not do" instead.
//!
//! The default below is the one `crates/duet-backend-macos/build.rs` hard-codes.
//! The two must move together; `FLUTTER_MACOS_FRAMEWORK_DIR` overrides both.

fn main() {
    println!("cargo:rerun-if-env-changed=FLUTTER_MACOS_FRAMEWORK_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let framework_dir = std::env::var("FLUTTER_MACOS_FRAMEWORK_DIR").unwrap_or_else(|_| {
        "/Users/kishan/dev/rkishan516/flutterDC/bin/cache/artifacts/engine/darwin-x64/\
         FlutterMacOS.xcframework/macos-arm64_x86_64"
            .to_string()
    });
    println!("cargo:rustc-link-arg=-Wl,-rpath,{framework_dir}");
}
