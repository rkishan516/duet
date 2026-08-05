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

/// The platform operations a `Host` performs.
///
/// This is a trait rather than a direct `tao`/`wry` dependency so that every
/// orchestration decision is testable with no window server present — the same
/// seam that let `duet-runtime` and `duet-supervisor` reach high coverage on any
/// machine. The real backend arrives in Phase 2b-3.
///
/// Implementations run on the main thread: Spike B established that `tao`'s
/// event loop, Flutter's platform thread and the webview all require it.
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
    fn start_renderer(&self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Attaches the surface's view to its window, making it render.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the view could not be attached.
    fn attach_view(&self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Detaches the surface's view, leaving the renderer alive.
    ///
    /// Cheap and cheaply reversed. Spike A measured that this reclaims
    /// essentially no memory — 223 MB before and after — which is why it is
    /// distinct from [`WindowBackend::destroy_renderer`].
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the view could not be detached.
    fn detach_view(&self, surface: SurfaceId) -> Result<(), BackendError>;

    /// Destroys the renderer entirely.
    ///
    /// **This is the operation that reclaims memory** — Spike A measured
    /// 223 MB before and 104 MB after on the Flutter side.
    ///
    /// # Errors
    ///
    /// [`BackendError::Unavailable`] if the renderer could not be destroyed.
    fn destroy_renderer(&self, surface: SurfaceId) -> Result<(), BackendError>;
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
/// Cloning shares the recording, so a clone can be handed to a `Host` while
/// the original is used to assert.
#[derive(Debug, Clone, Default)]
pub struct RecordingBackend {
    inner: Arc<Mutex<Recording>>,
}

#[derive(Debug, Default)]
struct Recording {
    calls: Vec<BackendCall>,
    fail_next: Option<BackendError>,
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
    fn start_renderer(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::StartRenderer(surface))
    }
    fn attach_view(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::AttachView(surface))
    }
    fn detach_view(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::DetachView(surface))
    }
    fn destroy_renderer(&self, surface: SurfaceId) -> Result<(), BackendError> {
        self.record(BackendCall::DestroyRenderer(surface))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_supervisor::SurfaceId;

    #[test]
    fn recording_backend_captures_calls_in_order() {
        let b = RecordingBackend::new();
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
        let clone = b.clone();
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
        let b = RecordingBackend::new();
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
        let b = RecordingBackend::new();
        b.fail_next(BackendError::Unavailable("transient".to_string()));
        assert!(b.start_renderer(SurfaceId::from_raw(4)).is_err());
        assert!(
            b.start_renderer(SurfaceId::from_raw(4)).is_ok(),
            "the failure must not be sticky"
        );
    }
}
