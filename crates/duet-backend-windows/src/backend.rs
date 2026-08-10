//! [`WinBackend`]: `duet_host::WindowBackend` over real `tao` windows and
//! real per-surface Flutter engines.

use std::collections::BTreeMap;

use duet_host::{BackendError, Readiness, WindowBackend};
use duet_supervisor::SurfaceId;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};

use crate::engine::FlutterEngine;

/// Owns `tao` windows and per-surface renderers, implementing
/// [`duet_host::WindowBackend`].
///
/// One Flutter engine per surface: on Windows the first (and only) view
/// controller takes ownership of its engine (see
/// [`crate::engine::FlutterEngine`]'s docs and
/// `spikes/spike-b-windows/FINDINGS.md` W-F1), so a second surface cannot
/// share the first's engine even if both happen to be Flutter — the same
/// one-engine-per-surface shape the macOS backend settled on for its own
/// platform's reasons.
///
/// # Windows are managed outside the trait
///
/// [`duet_host::WindowBackend`]'s four methods take only a [`SurfaceId`] —
/// deliberately, so orchestration logic in `duet-host` never needs a display
/// to be tested. But creating a `tao` window needs an
/// [`EventLoopWindowTarget`], which only exists for the lifetime of a
/// callback inside `tao`'s event loop and so cannot be threaded through
/// [`WindowBackend::start_renderer`] or captured ahead of time. This backend
/// therefore exposes [`WinBackend::open_window`] and
/// [`WinBackend::close_window`] as inherent methods, outside the trait, for
/// a driver (running inside the `tao` event loop) to call directly —
/// `open_window` before telling a [`duet_host::Host`] the surface's window
/// opened, `close_window` after telling it the window closed.
///
/// # Detach on Windows parks the view; the window may then close safely
///
/// `detach_view` reparents the Flutter view's HWND into the engine's hidden
/// parking window rather than destroying anything (see
/// `crate::engine::FlutterEngine::detach` for why the C API forces this
/// design). A consequence worth stating: once a surface is detached, its
/// `tao` window truly owns no Flutter resources any more, so
/// [`WinBackend::close_window`] after a Suspend/Teardown is exactly as safe
/// as on macOS — the Win32 destroy-children-with-the-window behavior cannot
/// reach a parked view.
///
/// # The run-loop-tick property, inherited from the macOS backend
///
/// The macOS backend documents that its caller must invoke `Host::tick()` at
/// most once per turn of the `tao` event loop, because Cocoa needs a run-loop
/// tick between a detach and the next attach on the same engine. The Windows
/// reattach path has no observed analog of that constraint — reparenting is
/// synchronous, and the spike's W2 probe detached and reattached the same
/// controller across separate turns without incident — but the caller
/// property itself still holds by construction here (see
/// `duet_supervisor::Supervisor::tick`: at most one action per surface per
/// tick), and drivers should keep honoring it rather than assuming Windows
/// is forgiving about something macOS is not.
pub struct WinBackend {
    /// Path to the Flutter `data` directory every booted engine reads its
    /// assets from (`flutter_assets/` + `icudtl.dat`).
    flutter_data_dir: String,
    /// Every surface's `tao` window, if one is currently open for it.
    windows: BTreeMap<SurfaceId, Window>,
    /// Every surface's Flutter engine, if one is currently running for it.
    engines: BTreeMap<SurfaceId, FlutterEngine>,
}

impl WinBackend {
    /// Creates a backend that boots every Flutter engine from the assets in
    /// `flutter_data_dir` (the `data` directory of a
    /// `flutter build windows --debug` output, e.g.
    /// `build/windows/x64/runner/Debug/data`).
    pub fn new(flutter_data_dir: impl Into<String>) -> Self {
        WinBackend {
            flutter_data_dir: flutter_data_dir.into(),
            windows: BTreeMap::new(),
            engines: BTreeMap::new(),
        }
    }

    /// Creates a `tao` window for `surface` and takes ownership of it.
    ///
    /// Call this — on the main thread, from inside the `tao` event loop —
    /// before reporting `HostEvent::WindowOpened` to a
    /// [`duet_host::Host`]: `attach_view` looks up the window this stores,
    /// and fails if none is open yet for the surface.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if `tao` could not create the window,
    /// or if a window is already open for this surface (close it first).
    pub fn open_window<T: 'static>(
        &mut self,
        surface: SurfaceId,
        target: &EventLoopWindowTarget<T>,
        title: &str,
    ) -> Result<(), BackendError> {
        if self.windows.contains_key(&surface) {
            return Err(BackendError::Unavailable(
                "a window is already open for this surface".to_string(),
            ));
        }
        let window = WindowBuilder::new()
            .with_title(title)
            .build(target)
            .map_err(|e| BackendError::Unavailable(format!("failed to build tao window: {e}")))?;
        self.windows.insert(surface, window);
        Ok(())
    }

    /// Drops the `tao` window owned for `surface`, if any.
    ///
    /// Call this after reporting `HostEvent::WindowClosed`, once the
    /// corresponding `Suspend`/`Teardown` has already detached the Flutter
    /// view — a detached view is parked outside this window's child chain, so
    /// the Win32 rule that destroying a window destroys its children cannot
    /// reach it (see this type's docs).
    ///
    /// Returns whether a window was actually removed.
    pub fn close_window(&mut self, surface: SurfaceId) -> bool {
        self.windows.remove(&surface).is_some()
    }

    /// Borrows the `tao` window backing `surface`, if one is currently open.
    pub fn window(&self, surface: SurfaceId) -> Option<&Window> {
        self.windows.get(&surface)
    }

    /// Borrows the Flutter engine running for `surface`, if any.
    ///
    /// The only way to obtain a [`FlutterEngine`] from outside this crate,
    /// and it exists for exactly one caller shape: building a
    /// [`crate::FlutterSurface`] against the engine
    /// [`WindowBackend::start_renderer`] just booted. `FlutterEngine`'s own
    /// methods are all `pub(crate)`, so a borrow handed out here cannot be
    /// used to boot, attach, detach or shut the engine down behind this
    /// backend's back — that stays the trait impl's business.
    pub fn engine(&self, surface: SurfaceId) -> Option<&FlutterEngine> {
        self.engines.get(&surface)
    }
}

impl WindowBackend for WinBackend {
    /// Boots a Flutter engine for `surface`.
    ///
    /// Returns [`Readiness::Ready`], never [`Readiness::Pending`]: the spike
    /// confirmed `FlutterDesktopEngineRun` is synchronous enough that the
    /// isolate is genuinely serving by the time it returns (W-F4 — the very
    /// first platform-channel message dispatched after `Run` was answered),
    /// so reporting `Pending` here would just add a spurious round trip
    /// through `Host::handle_at`. This is a per-renderer-kind decision, not a
    /// blanket one: a `wry` webview's creation is genuinely asynchronous on
    /// Windows (WebView2 environment setup) and would warrant `Pending`
    /// if/when this backend grows a webview surface kind — but that is not
    /// implemented in this crate yet (parity with the macOS backend, whose
    /// webview surfaces are likewise driven by examples directly).
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
        let engine = FlutterEngine::boot(&self.flutter_data_dir)?;
        self.engines.insert(surface, engine);
        Ok(Readiness::Ready)
    }

    /// Attaches the surface's Flutter view to its `tao` window.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if no window is open for this surface
    /// (see [`WinBackend::open_window`]), no renderer is running for it (see
    /// [`WindowBackend::start_renderer`]), or the attach itself fails.
    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        let window = self.windows.get(&surface).ok_or_else(|| {
            BackendError::Unavailable("no window open for this surface".to_string())
        })?;
        let engine = self.engines.get_mut(&surface).ok_or_else(|| {
            BackendError::Unavailable("no renderer running for this surface".to_string())
        })?;
        engine.attach(window)
    }

    /// Detaches the surface's Flutter view, leaving its engine running.
    ///
    /// On Windows "detach" parks the view HWND in the engine's hidden parking
    /// window — the controller and engine stay alive, which is what makes a
    /// later [`WindowBackend::attach_view`] cheap (see
    /// `crate::engine::FlutterEngine::detach`).
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if no renderer is running for this
    /// surface. Detaching when no view is attached is not an error — the
    /// underlying `FlutterEngine::detach` is a no-op in that case — only a
    /// missing renderer is.
    ///
    /// Also [`BackendError::Unavailable`] if `FlutterEngine::detach`'s own
    /// `flutter/lifecycle` sends failed — see its docs (macOS `FINDINGS.md`
    /// F1): the view is still parked either way, but the caller should know
    /// the backing-store retry storm this exists to prevent may not actually
    /// be prevented this time.
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        let engine = self.engines.get_mut(&surface).ok_or_else(|| {
            BackendError::Unavailable("no renderer running for this surface".to_string())
        })?;
        engine.detach()
    }

    /// Shuts the surface's engine down and forgets it.
    ///
    /// Idempotent: destroying a surface with no renderer succeeds without
    /// doing anything, rather than erroring. This matters because
    /// `duet_host::Host::perform` retries a failed destroy exactly once —
    /// if the second attempt found nothing to destroy and that counted as
    /// failure, a destroy whose *first* attempt actually succeeded but
    /// whose result was lost (for example a channel error unrelated to the
    /// engine itself) would be misreported as failed forever, even though
    /// the memory really was reclaimed.
    ///
    /// Does **not** close the surface's `tao` window — see
    /// [`WinBackend::close_window`], which the driver calls separately once
    /// it has told the `Host` the window closed.
    ///
    /// # Errors
    ///
    /// This implementation is currently infallible (`FlutterEngine::shutdown`
    /// absorbs teardown failures rather than propagating them — see its
    /// docs), but returns `Result` to satisfy the trait and leave room for a
    /// future check (e.g. verifying the shutdown actually completed).
    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        if let Some(mut engine) = self.engines.remove(&surface) {
            engine.shutdown();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_backend_has_no_windows_or_renderers() {
        // The only assertion this crate can make about `WinBackend` without
        // a main thread, a display session, or the Flutter engine DLL doing
        // real work (linking itself is exercised by every other test and by
        // `cargo build`, which fails loudly if the DLL is missing) —
        // everything else needs engine calls that require a real event loop.
        // See `examples/lifecycle.rs` for the real verification.
        let backend = WinBackend::new("Z:/nonexistent/data");
        assert!(
            backend.window(SurfaceId::from_raw(1)).is_none(),
            "a fresh backend must not have a window for a surface nobody opened"
        );
    }

    #[test]
    fn destroying_a_renderer_that_never_started_is_ok() {
        // The idempotence half of the destroy contract, reachable without a
        // display: the host retries a failed destroy exactly once, so
        // "nothing to destroy" must count as success.
        let mut backend = WinBackend::new("Z:/nonexistent/data");
        assert_eq!(backend.destroy_renderer(SurfaceId::from_raw(1)), Ok(()));
    }
}
