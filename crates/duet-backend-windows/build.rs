use std::path::{Path, PathBuf};
use std::process::Command;

/// Locates the Flutter Windows engine artifact directory — the one containing
/// `flutter_windows.dll`, its import library `flutter_windows.dll.lib`, and
/// `flutter_windows.h`.
///
/// `FLUTTER_WINDOWS_ENGINE_DIR` overrides everything. Otherwise the directory
/// is discovered from the `flutter` on PATH (docs/10-porting.md §8 calls
/// fixing the macOS crate's hard-coded default "a worthwhile first commit on
/// any machine that is not the one this was written on" — this crate starts
/// out with discovery instead of ever having a machine-specific default).
fn engine_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FLUTTER_WINDOWS_ENGINE_DIR") {
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
        .expect(
            "flutter is not on PATH; set FLUTTER_WINDOWS_ENGINE_DIR to the directory \
             containing flutter_windows.dll (usually <flutter>/bin/cache/artifacts/engine/windows-x64)",
        );
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
    if !dll.exists() {
        panic!(
            "flutter_windows.dll not found at {}. Set FLUTTER_WINDOWS_ENGINE_DIR to the engine \
             artifact directory, or run `flutter precache --windows` to populate the SDK cache.",
            dll.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", dir.display());
    // MSVC resolves this name to `flutter_windows.dll.lib`, the import library
    // the artifact cache ships next to the DLL.
    println!("cargo:rustc-link-lib=dylib=flutter_windows.dll");

    // Delay-load the engine DLL in every binary this crate builds.
    //
    // flutter_windows.dll statically links its CRT (its import table names no
    // ucrt/api-ms-win-crt DLL at all — verified with dumpbin), and a static
    // CRT snapshots the process environment once, at DLL load. Eagerly
    // linked, that load happens at process start — so the
    // `FLUTTER_ENGINE_SWITCHES` variables `duet_dev::engine_switches` has a
    // driver set in `main` (vm-service-port, disable-service-auth-codes) can
    // NEVER reach the engine's `std::getenv`: the snapshot predates `main`.
    // The hot_reload example found this the hard way — the VM service came up
    // on a random port with an auth code and the reload driver's connect was
    // refused. Delay-loading moves the DLL load (and with it the CRT's
    // environment snapshot) to the first engine call, which is after a driver
    // has set its switches. Binaries that never call the engine (the unit
    // tests) now also run without the DLL present at all.
    //
    // Library consumers building their own executables do not inherit these
    // flags (cargo link-args do not propagate across crates); an embedder that
    // needs engine switches must either pass the same /DELAYLOAD flags or set
    // the variables in the parent environment before its process starts.
    // (Examples only: cargo rejects the -bins/-tests directives for target
    // kinds this crate does not have, and the unit tests live inside the lib.)
    println!("cargo:rustc-link-arg-examples=/DELAYLOAD:flutter_windows.dll");
    println!("cargo:rustc-link-arg-examples=delayimp.lib");

    // Windows has no rpath. So that `cargo run --example ...` just works, copy
    // the DLL next to the built binaries (OUT_DIR is
    // target/<profile>/build/<pkg>-<hash>/out — three ancestors up is the
    // profile directory, and examples land in <profile>/examples too).
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR shallower than expected")
        .to_path_buf();
    // Copies can fail while a previously-built exe holds the DLL open; that is
    // fine — the DLL already there is the one it loaded.
    let _ = std::fs::copy(&dll, profile_dir.join("flutter_windows.dll"));
    // Examples run from <profile>/examples, and Windows resolves a DLL from
    // the exe's own directory first — created here because this build script
    // runs before any example has been built into it.
    let examples_dir = profile_dir.join("examples");
    let _ = std::fs::create_dir_all(&examples_dir);
    let _ = std::fs::copy(&dll, examples_dir.join("flutter_windows.dll"));
}
