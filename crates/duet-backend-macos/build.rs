use std::path::PathBuf;

fn main() {
    // Directory containing FlutterMacOS.xcframework's macos slice (the .framework lives directly
    // inside this dir). Allow override via env var for other machines/spikes.
    let framework_dir = std::env::var("FLUTTER_MACOS_FRAMEWORK_DIR").unwrap_or_else(|_| {
        "/Users/kishan/dev/rkishan516/flutterDC/bin/cache/artifacts/engine/darwin-x64/FlutterMacOS.xcframework/macos-arm64_x86_64".to_string()
    });

    let framework_path = PathBuf::from(&framework_dir).join("FlutterMacOS.framework");
    if !framework_path.exists() {
        panic!(
            "FlutterMacOS.framework not found at {}. Set FLUTTER_MACOS_FRAMEWORK_DIR to override.",
            framework_path.display()
        );
    }

    println!("cargo:rustc-link-search=framework={}", framework_dir);
    println!("cargo:rustc-link-lib=framework=FlutterMacOS");

    // Embed an rpath so the dynamic linker finds FlutterMacOS.framework at runtime without
    // needing DYLD_FRAMEWORK_PATH set manually (so `cargo run` just works).
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", framework_dir);

    println!("cargo:rerun-if-env-changed=FLUTTER_MACOS_FRAMEWORK_DIR");
}
