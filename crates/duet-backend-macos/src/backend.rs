//! [`MacBackend`]: `duet_host::WindowBackend` over real `tao` windows and
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
/// One Flutter engine per surface: Spike A confirmed an engine accepts only
/// one view controller at a time (see [`crate::engine::FlutterEngine`]'s
/// docs), so a second surface cannot share the first's engine even if both
/// happen to be Flutter.
///
/// # Windows are managed outside the trait
///
/// [`duet_host::WindowBackend`]'s four methods take only a [`SurfaceId`] —
/// deliberately, so orchestration logic in `duet-host` never needs a display
/// to be tested. But creating a `tao` window needs an
/// [`EventLoopWindowTarget`], which only exists for the lifetime of a
/// callback inside `tao`'s event loop and so cannot be threaded through
/// [`WindowBackend::start_renderer`] or captured ahead of time. This backend
/// therefore exposes [`MacBackend::open_window`] and
/// [`MacBackend::close_window`] as inherent methods, outside the trait, for
/// a driver (running inside the `tao` event loop) to call directly —
/// `open_window` before telling a [`duet_host::Host`] the surface's window
/// opened, `close_window` after telling it the window closed.
///
/// # The run-loop-tick requirement on detach → recreate
///
/// Spike A's constraint 5 says a view controller's `detach` and a fresh
/// `initWithEngine:` for the *same* engine must be separated by at least one
/// run-loop tick, even though the old controller was already dropped —
/// otherwise it races into constraint 3 (one view controller per engine at a
/// time), because Cocoa has not yet run the old controller's `dealloc`.
///
/// This backend does not add any explicit delay for that. Instead it relies
/// on a property of its caller, verified by reading the source rather than
/// assumed:
///
/// - `duet_supervisor::Supervisor::tick` (`crates/duet-supervisor/src/supervisor.rs`)
///   iterates every registered surface exactly once per call, and for each
///   calls a private `decide()` function whose body is a chain of
///   early-return `if`/`match` arms — the very first branch that applies
///   returns immediately. There is no path through `decide()` that produces
///   more than one [`duet_supervisor::SurfaceAction`] for the same surface.
/// - `duet_host::Host::tick` (`crates/duet-host/src/host.rs`) calls
///   `supervisor.tick(now)` once and then performs every returned action
///   synchronously, in order, within that same call.
///
/// Together: within one `Host::tick()` call, at most one of `Start`,
/// `Resume`, `Suspend`, `Teardown` is ever produced for a given surface —
/// so this backend's `detach_view` (driven by `Suspend`) and the
/// `attach_view` that later reattaches the *same* engine's view (driven by
/// `Resume`, since `Suspend` moves the surface to `Suspending` and only a
/// *later* `tick()` call can observe `Suspending` and decide `Resume` — see
/// `decide`'s `Suspending` arm) can only ever land in **separate**
/// `Host::tick()` calls. As long as a caller invokes `Host::tick()` at most
/// once per turn of the `tao` event loop — true of every reasonable driver,
/// including the Task 4 lifecycle example — that guarantees at least one
/// return to the run loop between the two, which is exactly the gap Cocoa
/// needs to run the old controller's `dealloc` before this backend creates
/// the new one. `MacBackend` itself does not and cannot enforce "at most
/// once per run-loop turn"; that is a property of the caller, recorded here
/// because it is load-bearing for constraint 5 and easy to violate silently
/// (for example, by calling `tick()` twice back-to-back to "catch up").
pub struct MacBackend {
    /// Path to the Flutter `App.framework` bundle every booted engine reads
    /// its assets from.
    app_framework: String,
    /// Every surface's `tao` window, if one is currently open for it.
    windows: BTreeMap<SurfaceId, Window>,
    /// Every surface's Flutter engine, if one is currently running for it.
    engines: BTreeMap<SurfaceId, FlutterEngine>,
}

impl MacBackend {
    /// Creates a backend that boots every Flutter engine from the assets in
    /// `app_framework` (a path to Flutter's `App.framework` bundle, as
    /// produced by `flutter build macos`).
    pub fn new(app_framework: impl Into<String>) -> Self {
        MacBackend {
            app_framework: app_framework.into(),
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
    /// view — dropping the window first would drop the view's superview out
    /// from under it.
    ///
    /// Returns whether a window was actually removed.
    pub fn close_window(&mut self, surface: SurfaceId) -> bool {
        self.windows.remove(&surface).is_some()
    }

    /// Borrows the `tao` window backing `surface`, if one is currently open.
    pub fn window(&self, surface: SurfaceId) -> Option<&Window> {
        self.windows.get(&surface)
    }
}

impl WindowBackend for MacBackend {
    /// Boots a Flutter engine for `surface`.
    ///
    /// Returns [`Readiness::Ready`], never [`Readiness::Pending`]: Spike A
    /// confirmed `runWithEntrypoint:` is synchronous — it returns only once
    /// the isolate is actually running — so by the time
    /// [`FlutterEngine::boot`] returns, the renderer genuinely is ready, and
    /// reporting `Pending` here would just add a spurious round trip through
    /// `Host::handle_at`. This is a per-renderer-kind decision, not a
    /// blanket one: a `wry` webview's `load_url` returns before the page has
    /// finished loading, which is exactly asynchronous and would warrant
    /// `Pending` if/when this backend grows a webview surface kind — but
    /// that is not implemented in this crate yet (see the crate root docs
    /// on scope), so `MacBackend` today only ever returns `Ready`.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if a renderer is already running for
    /// this surface, or if [`FlutterEngine::boot`] fails — see its docs.
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError> {
        if self.engines.contains_key(&surface) {
            return Err(BackendError::Unavailable(
                "a renderer is already running for this surface".to_string(),
            ));
        }
        let engine = FlutterEngine::boot(&self.app_framework)?;
        self.engines.insert(surface, engine);
        Ok(Readiness::Ready)
    }

    /// Attaches the surface's Flutter view to its `tao` window.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if no window is open for this surface
    /// (see [`MacBackend::open_window`]), no renderer is running for it (see
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
    /// See this type's docs for why the caller — not this method — is what
    /// guarantees a run-loop tick separates this from the `attach_view` that
    /// eventually reattaches the same engine.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if no renderer is running for this
    /// surface. Detaching when no view is attached is not an error — the
    /// underlying [`FlutterEngine::detach`] is a no-op in that case — only a
    /// missing renderer is.
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        let engine = self.engines.get_mut(&surface).ok_or_else(|| {
            BackendError::Unavailable("no renderer running for this surface".to_string())
        })?;
        engine.detach();
        Ok(())
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
    /// [`MacBackend::close_window`], which the driver calls separately once
    /// it has told the `Host` the window closed.
    ///
    /// # Errors
    ///
    /// This implementation is currently infallible ([`FlutterEngine::shutdown`]
    /// absorbs any Objective-C exception rather than propagating it — see
    /// its docs), but returns `Result` to satisfy the trait and leave room
    /// for a future check (e.g. verifying the shutdown actually completed).
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
        // The only assertion this crate can make about `MacBackend` without
        // a main thread, a window server, or a linked Flutter framework
        // present at *runtime* (linking itself is exercised by every other
        // test and by `cargo build`, which fails loudly if the framework is
        // missing) — everything else needs `AppKit`/`FlutterEngine` calls
        // that require a real event loop. See `examples/lifecycle.rs`
        // (Task 4) for the real verification.
        let backend = MacBackend::new("/nonexistent/App.framework");
        assert!(
            backend.window(SurfaceId::from_raw(1)).is_none(),
            "a fresh backend must not have a window for a surface nobody opened"
        );
    }
}
