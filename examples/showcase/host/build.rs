//! Gives this crate's binaries an rpath to the Flutter Linux engine.
//!
//! On Linux the backend links `libflutter_linux_gtk.so` out of the Flutter
//! SDK's artifact cache, and its own build script sets an rpath so *its*
//! examples run unaided — but a `cargo:rustc-link-arg` never reaches past the
//! package that emitted it, so the showcase and playground binaries would
//! link fine and then fail at startup with "cannot open shared object file".
//! The backend declares `links = "flutter_linux_gtk"` and exports the engine
//! directory it discovered; this script reads it back and arms the same
//! rpath for this package's binaries. macOS needs no analog (the framework
//! rpath travels differently) and Windows resolves its DLL through PATH, so
//! everywhere else this script does nothing.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        let dir = std::env::var("DEP_FLUTTER_LINUX_GTK_ENGINE_DIR")
            .expect("duet-backend-linux's build script should have exported the engine directory");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
