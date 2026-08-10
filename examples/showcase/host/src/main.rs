//! The Duet showcase host.
//!
//! One Rust process owns the store. A Flutter engine and a `wry` webview attach
//! to it as guests, read and write it, see each other's writes, and invoke the
//! host's commands. Halfway through, the Flutter guest is torn down — memory
//! comes back, the store does not notice — and then booted again, finding the
//! state it never saw written.
//!
//! ```console
//! $ (cd examples/showcase/flutter && flutter build macos --debug)   # or: flutter build windows --debug
//! $ (cd examples/showcase/web && npm install && npm run build)
//! $ cargo run -p duet-showcase
//! ```
//!
//! Environment:
//!
//! | Variable | Default |
//! |---|---|
//! | `DUET_APP_FRAMEWORK_PATH` (macOS) | `examples/showcase/flutter/build/macos/Build/Products/Debug/App.framework` |
//! | `DUET_FLUTTER_BUNDLE` (Windows) | `examples/showcase/flutter/build/windows/x64/runner/Debug/data` |
//! | `DUET_WEB_GUEST_PATH` | `examples/showcase/web/build/guest.js` |
//! | `DUET_SHOWCASE_LINGER_SECS` | `0` — set it to keep both guests alive for hot reload |
//!
//! See `examples/showcase/README.md` for what to look for, and for what could
//! not be verified on a machine with no reachable display.

#![deny(missing_docs)]

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    println!(
        "The Duet showcase's guests need a platform backend: a FlutterEngine plus a WKWebView \
         on macOS, or flutter_windows.dll plus WebView2 on Windows.\n\
         The definition half of this crate builds everywhere — try:\n\
         \n    cargo run -p duet-showcase --bin schema\n"
    );
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod tour;

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() {
    use std::time::Duration;

    use tao::event_loop::EventLoopBuilder;

    use crate::tour::backend::DuetEvent;

    #[cfg(target_os = "macos")]
    let framework = env_or(
        "DUET_APP_FRAMEWORK_PATH",
        "examples/showcase/flutter/build/macos/Build/Products/Debug/App.framework",
    );
    // The Windows analog of the App.framework: the `data` directory a debug
    // `flutter build windows` leaves next to the runner exe, holding
    // `flutter_assets/` and `icudtl.dat`.
    #[cfg(target_os = "windows")]
    let framework = env_or(
        "DUET_FLUTTER_BUNDLE",
        "examples/showcase/flutter/build/windows/x64/runner/Debug/data",
    );
    let bundle_path = env_or(
        "DUET_WEB_GUEST_PATH",
        "examples/showcase/web/build/guest.js",
    );
    let linger = Duration::from_secs(
        std::env::var("DUET_SHOWCASE_LINGER_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(0),
    );

    #[cfg(target_os = "macos")]
    println!("[showcase] Flutter App.framework: {framework}");
    #[cfg(target_os = "windows")]
    println!("[showcase] Flutter data dir:      {framework}");
    println!("[showcase] webview guest bundle:  {bundle_path}");

    // Read before anything boots: a missing bundle is a build step the user
    // skipped, and finding that out after two windows have opened is worse than
    // finding it out now.
    let bundle = match std::fs::read_to_string(&bundle_path) {
        Ok(text) => text,
        Err(e) => {
            println!("FAIL: setup — could not read the webview guest bundle: {e}");
            println!(
                "      Build it with:  (cd examples/showcase/web && npm install && npm run build)"
            );
            std::process::exit(1);
        }
    };

    let event_loop = EventLoopBuilder::<DuetEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let mut tour = match tour::setup(&event_loop, &framework, bundle, linger) {
        Ok(tour) => tour,
        Err(reason) => {
            println!("FAIL: setup — {reason}");
            std::process::exit(1);
        }
    };

    event_loop.run(move |event, target, control_flow| {
        tour::drive(&mut tour, event, target, &proxy, control_flow);
    });
}

/// The value of `name`, or `fallback` when it is unset.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}
