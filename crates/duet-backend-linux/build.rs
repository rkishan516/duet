use std::path::{Path, PathBuf};
use std::process::Command;

/// Locates the Flutter Linux engine artifact directory — the one containing
/// `libflutter_linux_gtk.so` and the `flutter_linux` headers.
///
/// `FLUTTER_LINUX_ENGINE_DIR` overrides everything; otherwise the directory
/// is discovered from the `flutter` on PATH, the same contract as the
/// Windows crate's build script (docs/10-porting.md §8 calls fixing the
/// macOS crate's hard-coded default "a worthwhile first commit" — the newer
/// backends start out with discovery).
fn engine_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FLUTTER_LINUX_ENGINE_DIR") {
        return PathBuf::from(dir);
    }
    let out = Command::new("which")
        .arg("flutter")
        .output()
        .expect("failed to run which to locate flutter; set FLUTTER_LINUX_ENGINE_DIR");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .expect(
            "flutter is not on PATH; set FLUTTER_LINUX_ENGINE_DIR to the directory \
             containing libflutter_linux_gtk.so (usually <flutter>/bin/cache/artifacts/engine/linux-x64)",
        );
    let bin = Path::new(first)
        .parent()
        .expect("flutter path has no parent directory");
    bin.join("cache")
        .join("artifacts")
        .join("engine")
        .join("linux-x64")
}

fn main() {
    println!("cargo:rerun-if-env-changed=FLUTTER_LINUX_ENGINE_DIR");

    let dir = engine_dir();
    let so = dir.join("libflutter_linux_gtk.so");
    if !so.exists() {
        panic!(
            "libflutter_linux_gtk.so not found at {}. Set FLUTTER_LINUX_ENGINE_DIR to the engine \
             artifact directory, or run `flutter precache --linux` to populate the SDK cache.",
            so.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=flutter_linux_gtk");
    // Linux has rpath, so `cargo run --example ...` just works with nothing
    // exported — the same convenience the macOS build.rs buys with its
    // framework rpath, and the thing Windows had to fake with DLL copies.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
}
