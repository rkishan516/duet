//! The Linux backend for Duet: real `tao` windows, a real Flutter engine
//! (`libflutter_linux_gtk.so`), and a real `wry` webview over WebKitGTK.
//!
//! Every other crate in this workspace — `duet-core`, `duet-runtime`,
//! `duet-codec`, `duet-supervisor`, `duet-host` — is platform-free by
//! design, so that orchestration can be tested on any machine. This crate is
//! the third that touches a real platform (after `duet-backend-macos`, the
//! reference implementation, and `duet-backend-windows` — the three are
//! written to be read side by side, per docs/10-porting.md §5.2): it
//! implements [`duet_host::WindowBackend`] over `tao` windows and
//! per-surface Flutter engines, and (in [`sink`]) [`duet_runtime::Sink`]
//! over `tao`'s `EventLoopProxy`.
//!
//! # What the Linux spike proved
//!
//! `spikes/spike-b-linux` (run under WSLg) booted the engine by realizing
//! `fl_view_new`'s implicit view inside a hidden window, attached it to a
//! visible `tao` window, ran a WebKitGTK `wry` webview beside it on the same
//! GTK main context, and drove both from a background thread through
//! `EventLoopProxy` for 180 s — 722/722 events, zero lost, completing the
//! trilogy (macOS 709/709, Windows 723/723). Its findings
//! (`spikes/spike-b-linux/FINDINGS.md`, L-F1..L-F7) shape everything here:
//!
//! - **Boot IS view realization** (L-F1/L-F2): there is no public
//!   `fl_engine_start`; the engine starts inside the implicit view's
//!   renderer realize, so `start_renderer` realizes the view into the
//!   surface's still-invisible window — which is why
//!   [`LinuxBackend::open_window`] must run first, and why the window is
//!   created hidden.
//! - **The view never moves** (L-F3): unrealizing the implicit view re-runs
//!   the engine start against a live engine and wrecks it. Attach/detach is
//!   `gtk_widget_show`/`hide` of the window the view has lived in since
//!   boot, and `close_window` under a live renderer is *deferred* — the
//!   window hides at once but is physically dropped only in
//!   `destroy_renderer` (see [`LinuxBackend`]).
//! - **Impeller must be off under a software rasterizer** (L-F4): one frame
//!   after boot, Impeller text display lists are FATAL on a Skia canvas.
//!   The engine boots with Impeller disabled; under WSLg drivers also need
//!   `FLUTTER_LINUX_RENDERER=software` in the environment (no usable GL
//!   there — "Could not determine GL version").
//! - **`wry` must build into the window's vbox** (L-F7): built against the
//!   `tao` window itself, the webview runs invisibly — the
//!   GtkApplicationWindow already holds its one permitted child.
//!
//! # Known limitation: verified under WSLg's software renderer
//!
//! This port was brought up and verified on WSLg — X11 over Wayland, no
//! DRI3, software rasterizer. On GL-capable desktops the embedder picks its
//! own renderer (this crate only disables Impeller, it does not force
//! software), but that configuration has not been exercised yet. Recorded
//! honestly rather than papered over.
//!
//! # Why this crate is not `#![forbid(unsafe_code)]`
//!
//! Every platform-free crate in the workspace forbids `unsafe`. This one
//! cannot: embedding the Flutter engine means calling its GObject C API
//! (`flutter_linux/*.h`, declared in the private `ffi` module) plus a few
//! raw `g_object_ref`/`unref` pairs for lifetimes gtk-rs cannot see. What
//! this crate promises instead:
//!
//! - Every `unsafe` block carries a `// SAFETY:` comment explaining why the
//!   call is valid at that point (right thread, live handle, checked null).
//! - No Rust panic may unwind across the C boundary: every registered
//!   callback body runs under [`std::panic::catch_unwind`] (see
//!   [`FlutterSurface`]), because unwinding into GObject signal-emission
//!   frames is undefined behavior — the same discipline as the other two
//!   backends, aimed at glib instead of Objective-C or MSVC C++.

#![deny(missing_docs)]

pub mod backend;
mod engine;
mod ffi;
mod flutter_surface;
pub mod sink;
mod webview;

pub use backend::LinuxBackend;
pub use engine::FlutterEngine;
pub use flutter_surface::{DUET_RPC_CHANNEL, FlutterSurface};
pub use sink::{DuetEvent, ProxySink};
pub use webview::WebviewSurface;
