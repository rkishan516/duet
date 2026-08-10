//! [`LinuxBackend`]: `duet_host::WindowBackend` over real `tao` windows and
//! real per-surface Flutter engines.

use std::collections::BTreeMap;

use duet_host::{BackendError, Readiness, WindowBackend};
use duet_supervisor::SurfaceId;
use glib::Cast;
use tao::event_loop::EventLoopWindowTarget;
use tao::platform::unix::WindowExtUnix;
use tao::window::{Window, WindowBuilder};

use crate::engine::{BootTarget, FlutterEngine};

/// A surface's `tao` window, in one of two lives.
enum WindowSlot {
    /// Open in the ordinary sense: the driver may attach into it, and
    /// [`LinuxBackend::window`] hands it out.
    Open(Window),
    /// The driver closed it while a renderer was still alive. It is hidden
    /// and logically gone — `window()` returns `None`, `attach_view` refuses
    /// — but physically kept until `destroy_renderer`, because dropping a
    /// `tao` window destroys its GTK children, and one of those children is
    /// the engine's realized `FlView` (destroying it out from under a live
    /// engine is the L-F3 wreck). The window is held only to keep it alive —
    /// nothing reads it again, hence the underscored field.
    Deferred { _window: Window },
}

/// Owns `tao` windows and per-surface renderers, implementing
/// [`duet_host::WindowBackend`].
///
/// One Flutter engine per surface, as on the other two platforms — here
/// because the engine is born inside its implicit view (`fl_view_new`), and
/// that view is realized into exactly one window for the engine's whole life
/// (see [`crate::engine::FlutterEngine`]).
///
/// # Windows are managed outside the trait
///
/// Same arrangement as macOS and Windows: [`duet_host::WindowBackend`]'s four
/// methods take only a [`SurfaceId`], so orchestration in `duet-host` stays
/// testable without a display, while creating a `tao` window needs an
/// [`EventLoopWindowTarget`] that only exists inside the event loop. Drivers
/// call [`LinuxBackend::open_window`] / [`LinuxBackend::close_window`]
/// directly.
///
/// # The two rules Linux adds
///
/// Both fall out of L-F1/L-F3 — the engine starts inside its view's realize,
/// and the realized view can never move or re-realize:
///
/// - **Open the window before `start_renderer`.** Boot realizes the view
///   *into the surface's window* (created invisible; `attach_view` is what
///   shows it). With no window open, boot falls back to a private holding
///   window and the engine is headless for life: it serves the store fine
///   (the state-sharing examples run this way on every platform) but
///   `attach_view` will refuse it forever.
/// - **`close_window` while a renderer is alive is deferred.** The window
///   hides and is logically closed at once — this method reports `true`, and
///   the ordinary `WindowClosed → Suspend/Teardown` host flow proceeds — but
///   the `tao` window object is physically dropped only inside
///   [`WindowBackend::destroy_renderer`], after engine shutdown has destroyed
///   the `FlView` living in it. On macOS the detach empties the window and on
///   Windows the view parks elsewhere, so both may drop the window at
///   `close_window`; on Linux the view stays put until the engine dies, so
///   the window's storage must outlive it.
///
/// # Detach hides the surface's window
///
/// On macOS a detached surface's window stays open, empty; on Windows too
/// (the view parks in a hidden sibling). On Linux `detach_view` unmaps the
/// window itself — hiding is the only unrealize-free way to take a `FlView`
/// off screen (L-F3). Drivers that suspend a surface while keeping its
/// window visible would see the whole window vanish; no current driver does
/// that (every Suspend in the examples follows the window closing), but it
/// is a visible platform difference worth knowing.
///
/// # The run-loop-tick property, inherited
///
/// As on the other platforms, drivers call `Host::tick()` at most once per
/// turn of the event loop. Linux's attach/detach (`gtk_widget_show`/`hide`)
/// gave no sign of needing turns between them in the spike's W2 cycle, but
/// the property holds by construction and drivers should keep honoring it.
pub struct LinuxBackend {
    /// Path to the Flutter `data` directory every booted engine reads its
    /// assets from (`flutter_assets/` + `icudtl.dat`).
    flutter_data_dir: String,
    /// Every surface's `tao` window, open or deferred.
    windows: BTreeMap<SurfaceId, WindowSlot>,
    /// Every surface's Flutter engine, if one is currently running for it.
    engines: BTreeMap<SurfaceId, FlutterEngine>,
}

impl LinuxBackend {
    /// Creates a backend that boots every Flutter engine from the assets in
    /// `flutter_data_dir` (the `data` directory of a
    /// `flutter build linux --debug` output, e.g.
    /// `build/linux/x64/debug/bundle/data`).
    pub fn new(flutter_data_dir: impl Into<String>) -> Self {
        LinuxBackend {
            flutter_data_dir: flutter_data_dir.into(),
            windows: BTreeMap::new(),
            engines: BTreeMap::new(),
        }
    }

    /// Creates a `tao` window for `surface` and takes ownership of it.
    ///
    /// The window is created **invisible** (`with_visible(false)`):
    /// [`WindowBackend::start_renderer`] realizes the Flutter view into it
    /// while it is unmapped — realization without mapping is exactly the
    /// headless-boot trick of the spike — and [`WindowBackend::attach_view`]
    /// is what first shows it. Call this on the main thread, from inside the
    /// `tao` event loop, **before** `start_renderer` (see the type docs), and
    /// before reporting `HostEvent::WindowOpened`.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `tao` could not create the window, or
    /// if a window is already held for this surface — including a deferred
    /// one: a close's physical teardown finishes inside `destroy_renderer`,
    /// so a driver reopening a surface must destroy the old renderer before
    /// opening the new window. Every supervisor-driven flow already does
    /// (`WindowClosed` triggers Suspend/Teardown before any reopen).
    pub fn open_window<T: 'static>(
        &mut self,
        surface: SurfaceId,
        target: &EventLoopWindowTarget<T>,
        title: &str,
    ) -> Result<(), BackendError> {
        if self.windows.contains_key(&surface) {
            return Err(BackendError::Unavailable(
                "a window is already held for this surface (a closed window is only \
                 released by destroy_renderer)"
                    .to_string(),
            ));
        }
        let window = WindowBuilder::new()
            .with_title(title)
            .with_visible(false)
            .build(target)
            .map_err(|e| BackendError::Unavailable(format!("failed to build tao window: {e}")))?;
        self.windows.insert(surface, WindowSlot::Open(window));
        Ok(())
    }

    /// Closes the `tao` window held for `surface` — logically always,
    /// physically only if no renderer is alive in it.
    ///
    /// With a live renderer the window hides and moves to the deferred state
    /// described on the type; without one it is dropped on the spot. Either
    /// way the surface has no open window afterwards, which is all the
    /// caller's host bookkeeping needs — call this after reporting
    /// `HostEvent::WindowClosed`, as on the other platforms.
    ///
    /// Returns whether a window was actually (logically) closed; closing a
    /// surface with no open window — including one already deferred — is
    /// `false`.
    pub fn close_window(&mut self, surface: SurfaceId) -> bool {
        match self.windows.remove(&surface) {
            Some(WindowSlot::Open(window)) => {
                if self.engines.contains_key(&surface) {
                    window.set_visible(false);
                    self.windows
                        .insert(surface, WindowSlot::Deferred { _window: window });
                } // else: dropping it here is safe — no FlView lives in it.
                true
            }
            Some(slot @ WindowSlot::Deferred { .. }) => {
                // Already logically closed; keep waiting for destroy_renderer.
                self.windows.insert(surface, slot);
                false
            }
            None => false,
        }
    }

    /// Borrows the `tao` window backing `surface`, if one is currently
    /// **open** — a deferred (logically closed) window is not handed out.
    pub fn window(&self, surface: SurfaceId) -> Option<&Window> {
        match self.windows.get(&surface) {
            Some(WindowSlot::Open(window)) => Some(window),
            _ => None,
        }
    }

    /// Borrows the Flutter engine running for `surface`, if any.
    ///
    /// The only way to obtain a [`FlutterEngine`] from outside this crate,
    /// for exactly one caller shape: building a [`crate::FlutterSurface`]
    /// against the engine [`WindowBackend::start_renderer`] just booted.
    /// `FlutterEngine`'s methods are all `pub(crate)`, so a borrow handed out
    /// here cannot drive the lifecycle behind this backend's back.
    pub fn engine(&self, surface: SurfaceId) -> Option<&FlutterEngine> {
        self.engines.get(&surface)
    }
}

impl WindowBackend for LinuxBackend {
    /// Boots a Flutter engine for `surface`, realizing its view into the
    /// surface's open window — or headless, into a private holding window,
    /// if none is open (see the type docs for why that engine can then never
    /// be attached).
    ///
    /// Returns [`Readiness::Ready`], never [`Readiness::Pending`]: the
    /// engine start runs inside the realize this call performs, and the
    /// spike verified Dart is serving platform-channel traffic by the time
    /// realize returns (L-F2) — same per-renderer-kind reasoning as the
    /// Windows backend's `Ready`.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if a renderer is already running for
    /// this surface, or if `FlutterEngine::boot` fails — see its docs.
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError> {
        if self.engines.contains_key(&surface) {
            return Err(BackendError::Unavailable(
                "a renderer is already running for this surface".to_string(),
            ));
        }
        let target = match self.windows.get(&surface) {
            Some(WindowSlot::Open(window)) => {
                BootTarget::Window(window.gtk_window().clone().upcast())
            }
            _ => BootTarget::Headless,
        };
        let engine = FlutterEngine::boot(&self.flutter_data_dir, target)?;
        self.engines.insert(surface, engine);
        Ok(Readiness::Ready)
    }

    /// Shows the surface's window and resumes the Flutter view living in it.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if no window is open for this surface
    /// (a deferred window counts as closed), no renderer is running for it,
    /// the renderer was booted headless (`FlutterEngine::attach` refuses —
    /// open the window before `start_renderer`), or a view is already
    /// attached.
    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        if self.window(surface).is_none() {
            return Err(BackendError::Unavailable(
                "no window open for this surface".to_string(),
            ));
        }
        let engine = self.engines.get_mut(&surface).ok_or_else(|| {
            BackendError::Unavailable("no renderer running for this surface".to_string())
        })?;
        engine.attach()
    }

    /// Detaches the surface's Flutter view, leaving its engine running.
    ///
    /// On Linux "detach" unmaps the window the view lives in — the
    /// unrealize-free way off screen (L-F3) — after the `flutter/lifecycle`
    /// sends that stop Dart's frame scheduling. The engine, and every watcher
    /// a guest armed, lives on; a later [`WindowBackend::attach_view`] is a
    /// `show` plus the resume sends.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if no renderer is running for this
    /// surface. Detaching when no view is attached is not an error — the
    /// underlying `FlutterEngine::detach` is a no-op in that case.
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        let engine = self.engines.get_mut(&surface).ok_or_else(|| {
            BackendError::Unavailable("no renderer running for this surface".to_string())
        })?;
        engine.detach()
    }

    /// Shuts the surface's engine down, forgets it, and completes any
    /// deferred window close.
    ///
    /// Order matters and is fixed here: `FlutterEngine::shutdown` first —
    /// it detaches (running the F1 lifecycle sends) and destroys the
    /// `FlView` widget — and only then, if the driver had already closed
    /// this surface's window, the `tao` window is dropped, now childless and
    /// safe. Dropping the window first would destroy the view under a live
    /// engine, the exact wreck the deferred-close design exists to prevent.
    ///
    /// Idempotent, for the same host-retry reason as the other backends:
    /// destroying a surface with no renderer succeeds without doing
    /// anything.
    ///
    /// Does **not** close a still-open window — see
    /// [`LinuxBackend::close_window`], which the driver calls separately.
    ///
    /// # Errors
    ///
    /// Currently infallible (`FlutterEngine::shutdown` absorbs teardown
    /// failures — see its docs); returns `Result` to satisfy the trait.
    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        if let Some(mut engine) = self.engines.remove(&surface) {
            engine.shutdown();
        }
        if let Some(WindowSlot::Deferred { .. }) = self.windows.get(&surface) {
            self.windows.remove(&surface);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE ON COVERAGE. Everything real about `LinuxBackend` needs a GTK
    // main context and a display; the assertions below are the bookkeeping
    // reachable without one. The full lifecycle — boot into an invisible
    // window, attach, detach, deferred close, destroy — is exercised live by
    // the crate's examples under WSLg.

    #[test]
    fn a_fresh_backend_has_no_windows_or_renderers() {
        let backend = LinuxBackend::new("/nonexistent/data");
        assert!(
            backend.window(SurfaceId::from_raw(1)).is_none(),
            "a fresh backend must not have a window for a surface nobody opened"
        );
        assert!(
            backend.engine(SurfaceId::from_raw(1)).is_none(),
            "a fresh backend must not have an engine for a surface nobody started"
        );
    }

    #[test]
    fn destroying_a_renderer_that_never_started_is_ok() {
        // The idempotence half of the destroy contract: the host retries a
        // failed destroy exactly once, so "nothing to destroy" must count as
        // success.
        let mut backend = LinuxBackend::new("/nonexistent/data");
        assert_eq!(backend.destroy_renderer(SurfaceId::from_raw(1)), Ok(()));
    }

    #[test]
    fn closing_a_window_nobody_opened_reports_false() {
        let mut backend = LinuxBackend::new("/nonexistent/data");
        assert!(!backend.close_window(SurfaceId::from_raw(1)));
    }

    #[test]
    fn starting_a_renderer_without_a_window_fails_on_assets_not_on_bookkeeping() {
        // Headless boot is a legal path, so the windowless case must reach
        // FlutterEngine::boot — which then refuses because the data
        // directory does not exist. If bookkeeping refused first, the error
        // would mention windows, not assets.
        let mut backend = LinuxBackend::new("/nonexistent/data");
        match backend.start_renderer(SurfaceId::from_raw(1)) {
            Err(BackendError::Unavailable(message)) => assert!(
                message.contains("flutter_assets"),
                "expected the assets check to fail, got: {message}"
            ),
            other => panic!("expected an assets failure, got {other:?}"),
        }
    }
}
