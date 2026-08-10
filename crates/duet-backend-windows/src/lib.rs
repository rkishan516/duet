//! The Windows backend for Duet: real `tao` windows, a real Flutter engine
//! (`flutter_windows.dll`), and a real `wry` webview over WebView2.
//!
//! Every other crate in this workspace — `duet-core`, `duet-runtime`,
//! `duet-codec`, `duet-supervisor`, `duet-host` — is platform-free by
//! design, so that orchestration can be tested on any machine. This crate is
//! the second that touches a real platform (the first is
//! `duet-backend-macos`, its reference implementation — the two are written
//! to be read side by side, per docs/10-porting.md §5.2): it implements
//! [`duet_host::WindowBackend`] over `tao` windows and per-surface Flutter
//! engines, and (in [`sink`]) [`duet_runtime::Sink`] over `tao`'s
//! `EventLoopProxy`.
//!
//! # What the Windows spike proved
//!
//! `spikes/spike-b-windows` booted the engine headless via
//! `FlutterDesktopEngineCreate`/`Run` (W-F3), attached a view controller to
//! the already-running engine and parented its HWND into a `tao` window,
//! ran a WebView2 `wry` webview beside it on the same thread, and drove both
//! from a background thread through `EventLoopProxy` for 180 s — 723/723
//! events, zero lost (the macOS Spike B measured 709/709). It also proved
//! the two facts that shape this crate's design:
//!
//! - **The view controller owns the engine** (W-F1): destroying it *is*
//!   engine shutdown (RSS 258 MB → 78 MB). So `detach` cannot destroy the
//!   controller the way macOS drops its `FlutterViewController`; it reparents
//!   the view HWND into a hidden parking window instead, and the full
//!   park/reattach cycle was verified live (W-F2).
//! - **Dart→native over `FlutterDesktopMessengerSetCallback` works** at
//!   sustained load (25,954 messages answered, W-F5), which is exactly the
//!   registration/reply pair [`FlutterSurface`] is built on.
//!
//! # Known limitation: top-level window messages are not forwarded
//!
//! A stock Flutter runner forwards its top-level window's messages to
//! `FlutterDesktopViewControllerHandleTopLevelWindowProc` (DPI changes, some
//! lifecycle nuances). `tao` owns the `WndProc` here and offers no hook for
//! that, and the spike ran correctly without it; window resize handling is
//! likewise the driver's job (the examples use fixed-size windows). Recorded
//! honestly rather than papered over — revisit if DPI-change handling or
//! resize tracking becomes a requirement.
//!
//! # Why this crate is not `#![forbid(unsafe_code)]`
//!
//! Every platform-free crate in the workspace forbids `unsafe`. This one
//! cannot: embedding the Flutter engine means calling its C API
//! (`flutter_windows.h`, declared in the private `ffi` module) and a handful
//! of Win32 window calls. What this crate promises instead:
//!
//! - Every `unsafe` block carries a `// SAFETY:` comment explaining why the
//!   call is valid at that point (right thread, live handle, checked null).
//! - No Rust panic may unwind across the C boundary: every registered
//!   callback body runs under [`std::panic::catch_unwind`] (see
//!   [`FlutterSurface`]), because unwinding into the engine's C++ frames is
//!   undefined behavior — the Windows analog of the macOS crate's
//!   Objective-C exception discipline, pointed the other direction.

#![deny(missing_docs)]

pub mod backend;
mod engine;
mod ffi;
mod flutter_surface;
pub mod sink;
mod webview;

pub use backend::WinBackend;
pub use engine::FlutterEngine;
pub use flutter_surface::{DUET_RPC_CHANNEL, FlutterSurface};
pub use sink::{DuetEvent, ProxySink};
pub use webview::WebviewSurface;
