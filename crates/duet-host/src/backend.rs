//! The platform operations a host needs, behind a trait so orchestration is
//! testable without a window server.

use std::sync::{Arc, Mutex};

use duet_supervisor::SurfaceId;

/// Why a platform operation could not be performed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendError {
    /// The platform could not satisfy the request — no display, no engine
    /// artifacts, or a renderer that failed to boot. Carries the reason.
    Unavailable(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unavailable(why) => write!(f, "platform operation failed: {why}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// Whether a renderer was ready by the time [`WindowBackend::start_renderer`]
/// returned.
///
/// A real Flutter engine boot takes roughly 180 ms (Spike A) and a webview
/// load is asynchronous, so a real backend cannot always answer synchronously.
/// This lets [`WindowBackend::start_renderer`] say so instead of forcing a
/// choice between blocking the main thread — freezing every other window —
/// or reporting success before the renderer is actually usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The renderer is up and its view can be attached immediately.
    Ready,
    /// The renderer is still starting.
    ///
    /// The backend must report [`duet_supervisor::HostEvent::Ready`] (or
    /// [`duet_supervisor::HostEvent::Failed`]) itself, through
    /// [`crate::Host::handle_at`], once the renderer settles. Until then the
    /// surface stays in `Starting` and the host does not attach a view for
    /// it.
    Pending,
}

/// The platform operations a [`crate::Host`] performs.
///
/// This is a trait rather than a direct `tao`/`wry` dependency so that every
/// orchestration decision is testable with no window server present — the same
/// seam that let `duet-runtime` and `duet-supervisor` reach high coverage on any
/// machine. The real backend arrives in Phase 2b-3.
///
/// Implementations run on the main thread: Spike B established that `tao`'s
/// event loop, Flutter's platform thread and the webview all require it.
///
/// Methods take `&mut self` rather than `&self`: a real backend owns `tao`
/// `Window` and `wry` `WebView` handles it must mutate to do this work, and
/// `&self` would force interior mutability (`RefCell`) throughout that
/// implementation for no benefit to this crate, which already gives a host
/// exclusive ownership of its backend.
pub trait WindowBackend {
    /// Creates a renderer for the surface — a Flutter engine or a webview.
    ///
    /// Spike A measured a cold Flutter engine boot at roughly 180 ms on a warm
    /// filesystem cache in a debug build.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the platform could not create it. The
    /// host reports this to the supervisor as a failure.
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError>;

    /// Attaches the surface's view to its window, making it render.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the view could not be attached.
    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Detaches the surface's view, leaving the renderer alive.
    ///
    /// Cheap and cheaply reversed. Spike A measured that this reclaims
    /// essentially no memory — 223 MB before and after — which is why it is
    /// distinct from [`WindowBackend::destroy_renderer`].
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the view could not be detached.
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Destroys the renderer entirely.
    ///
    /// **This is the operation that reclaims memory** — Spike A measured
    /// 223 MB before and 104 MB after on the Flutter side.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the renderer could not be destroyed.
    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError>;
}

/// One recorded call against a [`RecordingBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCall {
    /// [`WindowBackend::start_renderer`] was called.
    StartRenderer(SurfaceId),
    /// [`WindowBackend::attach_view`] was called.
    AttachView(SurfaceId),
    /// [`WindowBackend::detach_view`] was called.
    DetachView(SurfaceId),
    /// [`WindowBackend::destroy_renderer`] was called.
    DestroyRenderer(SurfaceId),
}

/// A backend that records every call instead of touching a platform.
///
/// Cloning shares the recording, so a clone can be handed to a [`crate::Host`] while
/// the original is used to assert.
#[derive(Debug, Clone, Default)]
pub struct RecordingBackend {
    inner: Arc<Mutex<Recording>>,
}

#[derive(Debug, Default)]
struct Recording {
    calls: Vec<BackendCall>,
    fail_next: Option<BackendError>,
    fail_next_attach: Option<BackendError>,
    pending_start: bool,
}

impl RecordingBackend {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call received, in order.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked. This is a
    /// test helper, so surfacing that loudly beats masking it.
    pub fn calls(&self) -> Vec<BackendCall> {
        self.inner
            .lock()
            .expect("recording lock poisoned")
            .calls
            .clone()
    }

    /// Makes the next call fail with `error`, then return to succeeding.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked.
    pub fn fail_next(&self, error: BackendError) {
        self.inner
            .lock()
            .expect("recording lock poisoned")
            .fail_next = Some(error);
    }

    /// Makes the next [`WindowBackend::start_renderer`] call report
    /// [`Readiness::Pending`], then return to [`Readiness::Ready`].
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked.
    pub fn start_pending(&self) {
        self.inner
            .lock()
            .expect("recording lock poisoned")
            .pending_start = true;
    }

    /// Makes the next [`WindowBackend::attach_view`] call fail with `error`,
    /// independently of [`RecordingBackend::fail_next`].
    ///
    /// `fail_next` fails whatever call comes next regardless of which method
    /// it is, which cannot target `attach_view` specifically when a
    /// preceding `start_renderer` in the same action must itself succeed —
    /// exactly the case a `Start` whose renderer boots but whose attach
    /// fails needs to test.
    ///
    /// # Panics
    ///
    /// Panics if a previous holder of the internal lock panicked.
    pub fn fail_next_attach(&self, error: BackendError) {
        self.inner
            .lock()
            .expect("recording lock poisoned")
            .fail_next_attach = Some(error);
    }

    fn record(&self, call: BackendCall) -> Result<(), BackendError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BackendError::Unavailable("recording lock poisoned".to_string()))?;
        // Record before failing: a host that retries must be able to see what
        // was attempted.
        inner.calls.push(call);
        match inner.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl WindowBackend for RecordingBackend {
    fn start_renderer(&mut self, surface: SurfaceId) -> Result<Readiness, BackendError> {
        self.record(BackendCall::StartRenderer(surface))?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BackendError::Unavailable("recording lock poisoned".to_string()))?;
        if std::mem::take(&mut inner.pending_start) {
            Ok(Readiness::Pending)
        } else {
            Ok(Readiness::Ready)
        }
    }
    fn attach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| BackendError::Unavailable("recording lock poisoned".to_string()))?;
        inner.calls.push(BackendCall::AttachView(surface));
        if let Some(e) = inner.fail_next_attach.take() {
            return Err(e);
        }
        match inner.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn detach_view(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::DetachView(surface))
    }
    fn destroy_renderer(&mut self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::DestroyRenderer(surface))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_supervisor::SurfaceId;

    #[test]
    fn recording_backend_captures_calls_in_order() {
        let mut b = RecordingBackend::new();
        b.start_renderer(SurfaceId::from_raw(1))
            .expect("start should succeed");
        b.attach_view(SurfaceId::from_raw(1))
            .expect("attach should succeed");
        b.destroy_renderer(SurfaceId::from_raw(1))
            .expect("destroy should succeed");

        assert_eq!(
            b.calls(),
            vec![
                BackendCall::StartRenderer(SurfaceId::from_raw(1)),
                BackendCall::AttachView(SurfaceId::from_raw(1)),
                BackendCall::DestroyRenderer(SurfaceId::from_raw(1)),
            ]
        );
    }

    #[test]
    fn clones_share_one_recording() {
        // The host takes the backend by value; a test needs a clone to assert on.
        let b = RecordingBackend::new();
        let mut clone = b.clone();
        clone
            .start_renderer(SurfaceId::from_raw(2))
            .expect("start should succeed");
        assert_eq!(
            b.calls().len(),
            1,
            "a clone and its original must share one log"
        );
    }

    #[test]
    fn a_failing_backend_reports_which_call_failed() {
        let mut b = RecordingBackend::new();
        b.fail_next(BackendError::Unavailable("no display".to_string()));
        let err = b
            .start_renderer(SurfaceId::from_raw(3))
            .expect_err("the primed failure must surface");
        assert_eq!(err, BackendError::Unavailable("no display".to_string()));
        // The failed call is still recorded — a host that retries must be able
        // to see what was attempted.
        assert_eq!(b.calls().len(), 1);
    }

    #[test]
    fn priming_a_failure_affects_only_the_next_call() {
        let mut b = RecordingBackend::new();
        b.fail_next(BackendError::Unavailable("transient".to_string()));
        assert!(b.start_renderer(SurfaceId::from_raw(4)).is_err());
        assert!(
            b.start_renderer(SurfaceId::from_raw(4)).is_ok(),
            "the failure must not be sticky"
        );
    }

    #[test]
    fn start_renderer_reports_ready_by_default() {
        let mut b = RecordingBackend::new();
        assert_eq!(
            b.start_renderer(SurfaceId::from_raw(5)),
            Ok(Readiness::Ready),
            "a fresh recorder must not require priming to report Ready"
        );
    }

    #[test]
    fn priming_pending_affects_only_the_next_start() {
        let mut b = RecordingBackend::new();
        b.start_pending();
        assert_eq!(
            b.start_renderer(SurfaceId::from_raw(6)),
            Ok(Readiness::Pending)
        );
        assert_eq!(
            b.start_renderer(SurfaceId::from_raw(6)),
            Ok(Readiness::Ready),
            "the pending state must not be sticky"
        );
    }
}
