//! Orchestrates the supervisor, the store and a platform backend.

use std::collections::BTreeMap;

use duet_core::{Instant, Policy, SubscriberId, SurfaceState};
use duet_runtime::StoreHandle;
use duet_supervisor::{HostEvent, Supervisor, SurfaceAction, SurfaceId};

use crate::backend::{BackendError, Readiness, WindowBackend};

/// Orchestrates the supervisor, the store and a platform backend.
///
/// Owns the `SurfaceId` → `SubscriberId` mapping, which exists nowhere else:
/// the supervisor has no store handle and the store knows nothing of surfaces.
#[derive(Debug)]
pub struct Host<B: WindowBackend> {
    /// Decides *when* each surface should start, suspend or be torn down.
    supervisor: Supervisor,
    /// The handle used to allocate subscribers and drop them on teardown.
    store: StoreHandle,
    /// The platform this host drives to carry out the supervisor's decisions.
    backend: B,
    /// Maps every registered surface to the subscriber it owns.
    subscribers: BTreeMap<SurfaceId, SubscriberId>,
}

impl<B: WindowBackend> Host<B> {
    /// Creates a host over a store handle and a platform backend.
    pub fn new(store: StoreHandle, backend: B) -> Self {
        Host {
            supervisor: Supervisor::new(),
            store,
            backend,
            subscribers: BTreeMap::new(),
        }
    }

    /// Registers a surface with its teardown policy.
    ///
    /// Allocates the surface a `SubscriberId` of its own. Two surfaces sharing
    /// one would have each other's notifications delivered to them, which
    /// crosses the trust boundary between the two guests.
    ///
    /// Registration is bookkeeping only — no renderer is created until a window
    /// opens and the supervisor decides to start it.
    pub fn register(&mut self, policy: Policy) -> SurfaceId {
        let id = self.supervisor.register(policy);
        self.subscribers.insert(id, self.store.next_subscriber_id());
        id
    }

    /// Forgets a surface entirely, returning whether it was registered.
    ///
    /// Releases the surface's store subscriptions, because the mapping that
    /// records them is discarded here and nothing could drop them afterwards.
    /// Does **not** destroy its renderer — call [`Host::request_suspend`] and
    /// let the policy reach teardown first if that is wanted.
    pub fn unregister(&mut self, id: SurfaceId) -> bool {
        self.drop_subscriptions(id);
        self.subscribers.remove(&id);
        self.supervisor.unregister(id)
    }

    /// The subscriber this surface owns, or `None` if it is not registered.
    pub fn subscriber_for(&self, id: SurfaceId) -> Option<SubscriberId> {
        self.subscribers.get(&id).copied()
    }

    /// The surface's lifecycle state, or `None` if it is not registered.
    pub fn state(&self, id: SurfaceId) -> Option<SurfaceState> {
        self.supervisor.state(id)
    }

    /// Borrows the store handle, for callers that need to read or subscribe.
    pub fn store_handle(&self) -> &StoreHandle {
        &self.store
    }

    /// Forwards a platform event to the supervisor.
    pub fn handle_at(&mut self, now: Instant, event: HostEvent) {
        self.supervisor.handle_at(now, event);
    }

    /// Advances the supervisor and performs whatever it decides.
    ///
    /// Returns the actions that were executed, which is what tests and logs
    /// assert on. An action whose backend call fails is still returned — the
    /// failure is reported to the supervisor separately, as a
    /// [`HostEvent::Failed`]. An action this build does not recognise —
    /// possible if a newer `duet-supervisor` adds a [`SurfaceAction`] variant
    /// — is ignored rather than treated as fatal, so one unfamiliar action
    /// cannot take every other surface down with it.
    pub fn tick(&mut self, now: Instant) -> Vec<SurfaceAction> {
        let actions = self.supervisor.tick(now);
        for action in &actions {
            self.perform(*action, now);
        }
        actions
    }

    /// Executes one action, closing the loop back to the supervisor.
    ///
    /// The supervisor cannot know whether a renderer actually came up, so the
    /// host reports [`HostEvent::Ready`] or [`HostEvent::Failed`] itself for
    /// [`SurfaceAction::Start`]. If the backend reports
    /// [`crate::backend::Readiness::Pending`] instead, nothing is attached and
    /// nothing is reported yet — the backend must call [`Host::handle_at`]
    /// itself once the renderer settles.
    ///
    /// A failed [`SurfaceAction::Suspend`] or [`SurfaceAction::Teardown`]
    /// additionally attempts [`WindowBackend::destroy_renderer`] before
    /// reporting the failure: a renderer left alive after a detach or destroy
    /// the host could not complete cleanly is a renderer whose memory is
    /// never reclaimed, which is worse than a redundant destroy attempt.
    ///
    /// # Residual gap
    ///
    /// A surface that reaches `Failed` has no automatic route back to `Live`.
    /// `HostEvent::Retry` moves `Failed` to `Starting`, but the supervisor
    /// only ever emits `Start` from `Cold`, so a retried surface wedges in
    /// `Starting` with no renderer started for it again. Recovering from
    /// `Failed` today requires the host to unregister and re-register the
    /// surface.
    fn perform(&mut self, action: SurfaceAction, now: Instant) {
        match action {
            SurfaceAction::Start(id) => self.perform_start(id, now),
            SurfaceAction::Resume(id) => self.perform_resume(id, now),
            SurfaceAction::Suspend(id) => self.perform_suspend(id, now),
            SurfaceAction::Teardown(id) => self.perform_teardown(id, now),
            // `SurfaceAction` is `#[non_exhaustive]`, so this arm is required
            // even though every variant that exists today is handled above.
            // Ignoring an unrecognised action is deliberate: this is the
            // host's dispatcher, and panicking here would take down every
            // surface in the process because a newer `duet-supervisor` grew
            // a variant this build does not know about.
            _ => {}
        }
    }

    /// Creates the renderer and, if it came up synchronously, attaches and
    /// reports [`HostEvent::Ready`]. See [`Host::perform`] for the `Pending`
    /// and failure paths.
    fn perform_start(&mut self, id: SurfaceId, now: Instant) {
        match self.backend.start_renderer(id) {
            Ok(Readiness::Ready) => match self.backend.attach_view(id) {
                Ok(()) => self.supervisor.handle_at(now, HostEvent::Ready(id)),
                Err(e) => self.report_failure(id, now, e),
            },
            // The backend will report Ready or Failed itself once the
            // renderer finishes booting; there is nothing to do here.
            Ok(Readiness::Pending) => {}
            Err(e) => self.report_failure(id, now, e),
        }
    }

    /// Reattaches a view to a renderer that never went away. Unlike `Start`,
    /// this needs no completion report: the supervisor already moved the
    /// surface to `Live` when it emitted the action.
    fn perform_resume(&mut self, id: SurfaceId, now: Instant) {
        if let Err(e) = self.backend.attach_view(id) {
            self.report_failure(id, now, e);
        }
    }

    /// Begins the grace period by detaching the view. A failed detach still
    /// attempts to destroy the renderer outright — see [`Host::perform`].
    fn perform_suspend(&mut self, id: SurfaceId, now: Instant) {
        if let Err(e) = self.backend.detach_view(id) {
            let _ = self.backend.destroy_renderer(id);
            self.report_failure(id, now, e);
        }
    }

    /// Drops the surface's subscriptions, then destroys its renderer,
    /// retrying the destroy once on failure — see [`Host::perform`].
    ///
    /// Subscriptions are dropped **before** the renderer is destroyed:
    /// reversing that order opens a window in which the store can still
    /// produce notifications for a surface whose renderer is already gone.
    fn perform_teardown(&mut self, id: SurfaceId, now: Instant) {
        self.drop_subscriptions(id);
        if let Err(e) = self.backend.destroy_renderer(id) {
            let _ = self.backend.destroy_renderer(id);
            self.report_failure(id, now, e);
        }
    }

    /// Reports a backend failure to the supervisor as [`HostEvent::Failed`].
    fn report_failure(&mut self, id: SurfaceId, now: Instant, error: BackendError) {
        self.supervisor
            .handle_at(now, HostEvent::Failed(id, error.to_string()));
    }

    /// Drops every store subscription belonging to a surface.
    ///
    /// `duet_supervisor::SurfaceAction::Teardown`'s docs make this the host's
    /// obligation: the supervisor has no store handle, and a missed drop
    /// leaves the store computing and delivering notifications for a
    /// renderer that no longer exists.
    ///
    /// A store error here is deliberately swallowed — if the runtime is
    /// already gone there is nothing to drop and nothing to recover.
    fn drop_subscriptions(&self, id: SurfaceId) {
        if let Some(subscriber) = self.subscribers.get(&id) {
            let _ = self.store.drop_subscriber(*subscriber);
        }
    }

    /// Asks a surface to suspend regardless of its policy, performing
    /// whatever the supervisor decides.
    pub fn request_suspend(&mut self, id: SurfaceId, now: Instant) -> Option<SurfaceAction> {
        let action = self.supervisor.request_suspend(id, now)?;
        self.perform(action, now);
        Some(action)
    }

    /// Asks a surface to resume regardless of its policy, performing
    /// whatever the supervisor decides.
    pub fn request_resume(&mut self, id: SurfaceId, now: Instant) -> Option<SurfaceAction> {
        let action = self.supervisor.request_resume(id, now)?;
        self.perform(action, now);
        Some(action)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::backend::{BackendCall, BackendError, RecordingBackend};
    use duet_core::{Policy, Value};
    use duet_runtime::{NullSink, Runtime};
    use duet_supervisor::WindowId;

    fn host() -> (Host<RecordingBackend>, RecordingBackend, Runtime) {
        let rt = Runtime::spawn(Value::map([("k", Value::Int(0))]), NullSink);
        let backend = RecordingBackend::new();
        let host = Host::new(rt.handle(), backend.clone());
        (host, backend, rt)
    }

    #[test]
    fn a_registered_surface_gets_its_own_subscriber_id() {
        let (mut h, _b, rt) = host();
        let a = h.register(Policy::Never);
        let b = h.register(Policy::Never);
        assert_ne!(
            h.subscriber_for(a),
            h.subscriber_for(b),
            "each surface must own a distinct subscriber, or their notifications cross"
        );
        assert!(h.subscriber_for(a).is_some());
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_unregistered_surface_has_no_subscriber() {
        let (h, _b, rt) = host();
        assert_eq!(h.subscriber_for(SurfaceId::from_raw(999)), None);
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn registering_does_not_touch_the_backend() {
        // Registration is bookkeeping. Nothing platform-facing happens until
        // a window opens and the supervisor decides to start the surface.
        let (mut h, b, rt) = host();
        h.register(Policy::Never);
        assert_eq!(b.calls(), vec![], "registration must not create a renderer");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn unregistering_releases_the_subscriber_mapping() {
        let (mut h, _b, rt) = host();
        let id = h.register(Policy::Never);
        assert!(h.unregister(id));
        assert_eq!(h.subscriber_for(id), None);
        assert!(!h.unregister(id), "a second unregister reports absence");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn unregistering_drops_the_surfaces_subscriptions() {
        // Once unregistered, the SurfaceId -> SubscriberId mapping is gone
        // and the supervisor no longer tracks the surface, so no future tick
        // can ever produce a Teardown for it. If unregister does not drop the
        // subscriptions itself, nothing ever will.
        let (mut h, _b, rt) = host();
        let id = h.register(Policy::Never);
        let subscriber = h.subscriber_for(id).expect("registered");
        let store = h.store_handle().clone();
        store
            .subscribe(subscriber, duet_core::Path::root())
            .expect("subscribe should succeed");
        store
            .subscribe(subscriber, duet_core::Path::root())
            .expect("subscribe should succeed");

        assert!(h.unregister(id));

        assert_eq!(
            store
                .drop_subscriber(subscriber)
                .expect("query should succeed"),
            0,
            "unregister must already have dropped both subscriptions"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn opening_a_window_starts_the_surface_and_attaches_its_view() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        let w = WindowId::new(1);

        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));

        assert_eq!(
            b.calls(),
            vec![BackendCall::StartRenderer(id), BackendCall::AttachView(id)],
            "a cold start must create the renderer and then attach"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_successful_start_reports_ready_to_the_supervisor() {
        let (mut h, _b, rt) = host();
        let id = h.register(Policy::Never);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        h.tick(Instant(0));
        assert_eq!(
            h.state(id),
            Some(SurfaceState::Live),
            "the host must close the loop by reporting Ready itself"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_failed_start_reports_failure_and_does_not_attach() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        b.fail_next(BackendError::Unavailable("no display".to_string()));

        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        h.tick(Instant(0));

        assert_eq!(
            b.calls(),
            vec![BackendCall::StartRenderer(id)],
            "a failed start must not be followed by an attach"
        );
        assert!(
            matches!(h.state(id), Some(SurfaceState::Failed(_))),
            "the failure must reach the supervisor, got {:?}",
            h.state(id)
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_successful_start_whose_attach_fails_still_reports_failure() {
        // The renderer boots fine; only the attach fails. The host must
        // still close the loop, even though the failure happened one step
        // later than a plain start_renderer failure.
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        b.fail_next_attach(BackendError::Unavailable("attach failed".to_string()));

        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        h.tick(Instant(0));

        assert_eq!(
            b.calls(),
            vec![BackendCall::StartRenderer(id), BackendCall::AttachView(id)],
            "the renderer must still have been created even though the attach failed"
        );
        assert!(
            matches!(h.state(id), Some(SurfaceState::Failed(_))),
            "an attach failure after a successful start must still reach the supervisor, got {:?}",
            h.state(id)
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn suspending_detaches_the_view_but_keeps_the_renderer() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));

        h.handle_at(
            Instant(100),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(100));

        assert_eq!(
            b.calls(),
            vec![
                BackendCall::StartRenderer(id),
                BackendCall::AttachView(id),
                BackendCall::DetachView(id),
            ],
            "suspend detaches only — Spike A measured that destroying is what frees memory"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn teardown_destroys_the_renderer_and_drops_the_subscriber() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 0 });
        let w = WindowId::new(1);
        let subscriber = h
            .subscriber_for(id)
            .expect("registered surfaces have subscribers");

        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        // Clone the handle: holding a borrow of `h` here would conflict with
        // the `&mut h` needed by `handle_at`/`tick` below.
        let store = h.store_handle().clone();
        store
            .subscribe(subscriber, duet_core::Path::root())
            .expect("subscribe should succeed");

        h.handle_at(
            Instant(10),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(10));
        h.tick(Instant(11));

        assert!(
            b.calls().contains(&BackendCall::DestroyRenderer(id)),
            "teardown must destroy the renderer, got {:?}",
            b.calls()
        );
        assert_eq!(
            store
                .drop_subscriber(subscriber)
                .expect("query should succeed"),
            0,
            "the host must already have dropped this surface's subscriptions"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_failed_detach_still_attempts_to_destroy_the_renderer() {
        // The policy fired specifically to reclaim memory; a transient
        // detach failure must not mean it is never freed.
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 1_000 });
        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        assert_eq!(h.state(id), Some(SurfaceState::Live));

        b.fail_next(BackendError::Unavailable("detach failed".to_string()));
        h.handle_at(
            Instant(100),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(100));

        assert!(
            b.calls().contains(&BackendCall::DestroyRenderer(id)),
            "a failed detach must still attempt to destroy the renderer, got {:?}",
            b.calls()
        );
        assert!(
            matches!(h.state(id), Some(SurfaceState::Failed(_))),
            "the failure must still reach the supervisor, got {:?}",
            h.state(id)
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_failed_destroy_is_retried_exactly_once() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 0 });
        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        h.handle_at(
            Instant(10),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(10));

        b.fail_next(BackendError::Unavailable("destroy failed".to_string()));
        h.tick(Instant(11));

        let destroys = b
            .calls()
            .iter()
            .filter(|c| matches!(c, BackendCall::DestroyRenderer(_)))
            .count();
        assert_eq!(
            destroys,
            2,
            "a failed destroy must be retried exactly once, got {:?}",
            b.calls()
        );
        assert!(
            matches!(h.state(id), Some(SurfaceState::Failed(_))),
            "the failure must still reach the supervisor, got {:?}",
            h.state(id)
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn resume_attaches_without_starting_a_new_renderer() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 10_000 });
        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        h.handle_at(
            Instant(100),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(100));

        // Reopen well inside the grace window.
        h.handle_at(
            Instant(200),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(200));

        let starts = b
            .calls()
            .iter()
            .filter(|c| matches!(c, BackendCall::StartRenderer(_)))
            .count();
        assert_eq!(
            starts, 1,
            "reattaching must not boot a second engine — Spike A measured that at ~180 ms"
        );
        assert_eq!(
            b.calls().last(),
            Some(&BackendCall::AttachView(id)),
            "resume ends in an attach"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_failed_resume_reports_failure() {
        // Resume needs no completion report on success — the supervisor
        // already moved the surface to Live — but a failed reattach must
        // still surface, or the surface is silently stuck without a view.
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 10_000 });
        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        h.handle_at(
            Instant(100),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(100));
        assert!(
            matches!(h.state(id), Some(SurfaceState::Suspending { .. })),
            "the grace period must still be open, got {:?}",
            h.state(id)
        );

        // Resume's attach_view is the first backend call in this flow (no
        // preceding start_renderer), so the call-agnostic `fail_next`
        // reaches it directly.
        b.fail_next(BackendError::Unavailable("reattach failed".to_string()));
        h.handle_at(
            Instant(200),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(200));

        assert!(
            matches!(h.state(id), Some(SurfaceState::Failed(_))),
            "a failed resume must still reach the supervisor, got {:?}",
            h.state(id)
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn request_suspend_detaches_a_live_surface_and_reports_the_action() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        h.tick(Instant(0));
        assert_eq!(h.state(id), Some(SurfaceState::Live));

        let action = h.request_suspend(id, Instant(1));

        assert_eq!(
            action,
            Some(SurfaceAction::Suspend(id)),
            "a manual suspend on a Live surface must report the Suspend action"
        );
        assert_eq!(
            b.calls().last(),
            Some(&BackendCall::DetachView(id)),
            "the manual suspend must actually detach the view"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn request_suspend_on_a_non_live_surface_is_a_no_op() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        // Never opened, so the surface is still Cold, not Live.

        let action = h.request_suspend(id, Instant(0));

        assert_eq!(
            action, None,
            "suspending only ever applies to a Live surface"
        );
        assert_eq!(b.calls(), vec![], "a no-op must not touch the backend");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn request_resume_reattaches_a_suspending_surface_and_reports_the_action() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 10_000 });
        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        h.handle_at(
            Instant(1),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(1));
        assert!(
            matches!(h.state(id), Some(SurfaceState::Suspending { .. })),
            "the grace period must still be open, got {:?}",
            h.state(id)
        );

        let action = h.request_resume(id, Instant(2));

        assert_eq!(
            action,
            Some(SurfaceAction::Resume(id)),
            "a manual resume during the grace period must report the Resume action"
        );
        assert_eq!(
            b.calls().last(),
            Some(&BackendCall::AttachView(id)),
            "the manual resume must actually reattach the view"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn request_resume_on_a_non_suspending_surface_is_a_no_op() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        h.tick(Instant(0));
        assert_eq!(h.state(id), Some(SurfaceState::Live));

        let action = h.request_resume(id, Instant(1));

        assert_eq!(
            action, None,
            "resuming only ever cancels a pending suspension"
        );
        assert_eq!(
            b.calls(),
            vec![BackendCall::StartRenderer(id), BackendCall::AttachView(id)],
            "a no-op must not perform any further backend call"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_pending_start_leaves_the_surface_starting_until_the_backend_reports_ready() {
        let (mut h, b, rt) = host();
        let id = h.register(Policy::Never);
        b.start_pending();

        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: WindowId::new(1),
            },
        );
        h.tick(Instant(0));

        assert_eq!(
            b.calls(),
            vec![BackendCall::StartRenderer(id)],
            "a pending start must not attach until the backend says it is ready"
        );
        assert_eq!(
            h.state(id),
            Some(SurfaceState::Starting),
            "the surface must stay Starting while the renderer is still booting, got {:?}",
            h.state(id)
        );

        // The backend finishes booting later and reports back through the
        // same event path a platform event would use.
        h.handle_at(Instant(50), HostEvent::Ready(id));

        assert_eq!(
            h.state(id),
            Some(SurfaceState::Live),
            "reporting Ready once the renderer settles must reach Live"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    /// A backend that inspects the store from inside `destroy_renderer`.
    ///
    /// This is the only way to observe the drop-before-destroy interleaving
    /// in [`Host::perform_teardown`]: by the time `tick` returns, both
    /// orderings look identical from outside, since `RecordingBackend` cannot
    /// see the store and the store cannot see the backend.
    #[derive(Debug, Clone)]
    struct StoreProbingBackend {
        store: StoreHandle,
        subscriber: Arc<Mutex<Option<SubscriberId>>>,
        seen: Arc<Mutex<Option<usize>>>,
    }

    impl StoreProbingBackend {
        fn new(store: StoreHandle) -> Self {
            StoreProbingBackend {
                store,
                subscriber: Arc::new(Mutex::new(None)),
                seen: Arc::new(Mutex::new(None)),
            }
        }

        /// Tells the probe which subscriber to query on the next
        /// `destroy_renderer` call. Separate from `new` because the real
        /// `SubscriberId` is only allocated once the surface is registered,
        /// which needs a `Host`, which needs this backend already built.
        fn watch(&self, subscriber: SubscriberId) {
            *self.subscriber.lock().expect("lock poisoned") = Some(subscriber);
        }

        /// The subscription count `destroy_renderer` observed, if it has run.
        fn seen(&self) -> Option<usize> {
            *self.seen.lock().expect("lock poisoned")
        }
    }

    impl WindowBackend for StoreProbingBackend {
        fn start_renderer(&mut self, _surface: SurfaceId) -> Result<Readiness, BackendError> {
            Ok(Readiness::Ready)
        }
        fn attach_view(&mut self, _surface: SurfaceId) -> Result<(), BackendError> {
            Ok(())
        }
        fn detach_view(&mut self, _surface: SurfaceId) -> Result<(), BackendError> {
            Ok(())
        }
        fn destroy_renderer(&mut self, _surface: SurfaceId) -> Result<(), BackendError> {
            if let Some(subscriber) = *self.subscriber.lock().expect("lock poisoned") {
                let remaining = self
                    .store
                    .drop_subscriber(subscriber)
                    .expect("query should succeed");
                *self.seen.lock().expect("lock poisoned") = Some(remaining);
            }
            Ok(())
        }
    }

    #[test]
    fn teardown_drops_subscriptions_before_destroying_the_renderer() {
        let rt = Runtime::spawn(Value::map([("k", Value::Int(0))]), NullSink);
        let backend = StoreProbingBackend::new(rt.handle());
        let mut h = Host::new(rt.handle(), backend.clone());
        let id = h.register(Policy::OnLastWindowClosed { grace_ms: 0 });
        let subscriber = h.subscriber_for(id).expect("registered");
        backend.watch(subscriber);

        let store = h.store_handle().clone();
        store
            .subscribe(subscriber, duet_core::Path::root())
            .expect("subscribe should succeed");

        let w = WindowId::new(1);
        h.handle_at(
            Instant(0),
            HostEvent::WindowOpened {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(0));
        h.handle_at(
            Instant(10),
            HostEvent::WindowClosed {
                surface: id,
                window: w,
            },
        );
        h.tick(Instant(10));
        h.tick(Instant(11));

        assert_eq!(
            backend.seen(),
            Some(0),
            "destroy_renderer must observe the subscription already gone, \
             proving the drop happened before the destroy"
        );
        rt.shutdown().expect("shutdown should succeed");
    }
}
