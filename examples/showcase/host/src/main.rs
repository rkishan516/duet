//! The Duet showcase host.
//!
//! One Rust process owns the store. A Flutter engine and a `wry` webview attach
//! to it as guests, read and write it, see each other's writes, and invoke the
//! host's commands. Halfway through, the Flutter guest is torn down — memory
//! comes back, the store does not notice — and then booted again, finding the
//! state it never saw written.
//!
//! ```console
//! $ (cd examples/showcase/flutter && flutter build macos --debug)
//! $ (cd examples/showcase/web && npm install && npm run build)
//! $ cargo run -p duet-showcase
//! ```
//!
//! Environment:
//!
//! | Variable | Default |
//! |---|---|
//! | `DUET_APP_FRAMEWORK_PATH` | `examples/showcase/flutter/build/macos/Build/Products/Debug/App.framework` |
//! | `DUET_WEB_GUEST_PATH` | `examples/showcase/web/build/guest.js` |
//! | `DUET_SHOWCASE_LINGER_SECS` | `0` — set it to keep both guests alive for hot reload |
//!
//! See `examples/showcase/README.md` for what to look for, and for what could
//! not be verified on a machine with no reachable display.

#![deny(missing_docs)]

#[cfg(not(target_os = "macos"))]
fn main() {
    println!(
        "The Duet showcase's guests are macOS-only: they need a FlutterEngine and a WKWebView.\n\
         The definition half of this crate builds everywhere — try:\n\
         \n    cargo run -p duet-showcase --bin schema\n"
    );
}

#[cfg(target_os = "macos")]
mod tour;

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Duration;

    use duet_backend_macos::DuetEvent;
    use tao::event_loop::EventLoopBuilder;

    let framework = env_or(
        "DUET_APP_FRAMEWORK_PATH",
        "examples/showcase/flutter/build/macos/Build/Products/Debug/App.framework",
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

    println!("[showcase] Flutter App.framework: {framework}");
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
#[cfg(target_os = "macos")]
fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}
