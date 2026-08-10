//! Links the spike against the Flutter Linux embedder
//! (`libflutter_linux_gtk.so`) from the Flutter SDK's engine artifact cache.
//!
//! Same discovery contract as the Windows spike: the SDK location comes from
//! the `flutter` on PATH, and `FLUTTER_LINUX_ENGINE_DIR` overrides it.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn engine_dir() -> PathBuf {
    if let Ok(dir) = env::var("FLUTTER_LINUX_ENGINE_DIR") {
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
        .expect("flutter is not on PATH; set FLUTTER_LINUX_ENGINE_DIR to the linux-x64 engine artifact directory");
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
    assert!(
        so.exists(),
        "libflutter_linux_gtk.so not found at {} — set FLUTTER_LINUX_ENGINE_DIR, or run `flutter precache --linux`",
        so.display()
    );
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=dylib=flutter_linux_gtk");
    // Linux has rpath, so `cargo run` just works with nothing exported —
    // the same convenience the macOS build.rs buys with its framework rpath.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
}
