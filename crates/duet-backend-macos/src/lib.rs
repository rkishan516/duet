//! The macOS backend for Duet: real `tao` windows, a real Flutter engine,
//! and (eventually) a real `wry` webview.
//!
//! Every other crate in this workspace — `duet-core`, `duet-runtime`,
//! `duet-codec`, `duet-supervisor`, `duet-host` — is platform-free by
//! design, so that orchestration can be tested on any machine. This crate is
//! the first that touches a real platform: it implements
//! [`duet_host::WindowBackend`] over `tao` windows and per-surface Flutter
//! engines, and (in [`sink`]) [`duet_runtime::Sink`] over `tao`'s
//! `EventLoopProxy`.
//!
//! # What Spikes A and B proved
//!
//! Spike A (`spikes/spike-a-macos`) booted a headless `FlutterEngine`,
//! attached and detached views against it, reused the same engine across
//! view replacement cycles, and measured that `shutDownEngine` — not
//! detaching a view — is what reclaims memory (223 MB attached, 104 MB after
//! shutdown). Spike B (`spikes/spike-b-macos`) proved `tao`'s event loop, a
//! Flutter platform thread and a `wry` webview coexist on one main thread,
//! and that `EventLoopProxy` reliably marshals work onto it (709/709 events
//! over 180 s). This crate ports that proven code rather than re-deriving
//! it; see `spikes/spike-a-macos/FINDINGS.md` and
//! `spikes/spike-b-macos/FINDINGS.md` for the full writeups.
//!
//! # No on-screen verification here
//!
//! This machine has no reachable on-screen WindowServer for spawned
//! processes: windows are created and Flutter/AppKit render into them, but
//! nothing appears on a physical display and no human can interact with
//! them. What *can* be verified without a display — linking, in-process
//! rasterization via `cacheDisplayInRect:toBitmapImageRep:`, and RSS before
//! and after teardown — is exercised by `examples/lifecycle.rs`. Real
//! keyboard/mouse input and anything a human would judge by looking at a
//! screen are out of scope for this environment; that is reported honestly
//! rather than claimed.
//!
//! # Why this crate is not `#![forbid(unsafe_code)]`
//!
//! Every other crate in the workspace forbids `unsafe`. This one cannot:
//! embedding the Flutter engine means calling Objective-C through `objc2`,
//! which is inherently `unsafe`. What this crate promises instead:
//!
//! - Every `unsafe` block carries a `// SAFETY:` comment explaining why the
//!   call is valid at that point (main thread, live object, correct type).
//! - `objc2`'s `"catch-all"` feature is enabled (see the workspace
//!   `Cargo.toml`), so an Objective-C exception becomes a Rust panic instead
//!   of aborting the process with no message. Fallible ObjC calls are then
//!   wrapped in [`std::panic::catch_unwind`] and turned into a
//!   [`duet_host::BackendError`] — never left to panic across an API
//!   boundary.

#![deny(missing_docs)]

pub mod backend;
mod engine;
pub mod sink;
mod webview;

pub use backend::MacBackend;
pub use sink::{DuetEvent, ProxySink};
pub use webview::WebviewSurface;
