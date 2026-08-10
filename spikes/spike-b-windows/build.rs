//! Links the spike against the Flutter Windows embedder (`flutter_windows.dll`)
//! from the Flutter SDK's engine artifact cache.
//!
//! Per docs/10-porting.md §8's note about fresh clones: the SDK location is
//! *discovered* from the `flutter` on PATH rather than hard-coded to one
//! machine's path. `FLUTTER_WINDOWS_ENGINE_DIR` overrides discovery entirely.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn engine_dir() -> PathBuf {
    if let Ok(dir) = env::var("FLUTTER_WINDOWS_ENGINE_DIR") {
        return PathBuf::from(dir);
    }
    let out = Command::new("where.exe")
        .arg("flutter")
        .output()
        .expect("failed to run where.exe to locate flutter; set FLUTTER_WINDOWS_ENGINE_DIR");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .expect("flutter is not on PATH; set FLUTTER_WINDOWS_ENGINE_DIR to the windows-x64 engine artifact directory");
    let bin = Path::new(first)
        .parent()
        .expect("flutter path has no parent directory");
    bin.join("cache")
        .join("artifacts")
        .join("engine")
        .join("windows-x64")
}

fn main() {
    println!("cargo:rerun-if-env-changed=FLUTTER_WINDOWS_ENGINE_DIR");
    let dir = engine_dir();
    let dll = dir.join("flutter_windows.dll");
    assert!(
        dll.exists(),
        "flutter_windows.dll not found at {} — set FLUTTER_WINDOWS_ENGINE_DIR, or run `flutter precache --windows`",
        dll.display()
    );
    println!("cargo:rustc-link-search=native={}", dir.display());
    // MSVC resolves this to `flutter_windows.dll.lib`, the import library the
    // artifact cache ships next to the DLL.
    println!("cargo:rustc-link-lib=dylib=flutter_windows.dll");

    // The DLL must be findable at runtime; drop a copy next to the built exe
    // (OUT_DIR = target/<profile>/build/<pkg>-<hash>/out, three levels down).
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR shallower than expected");
    let _ = std::fs::copy(&dll, profile_dir.join("flutter_windows.dll"));
}
