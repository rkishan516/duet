//! Orchestrates the supervisor, the store and a platform backend.

use std::collections::BTreeMap;

use duet_core::{Instant, Policy, SubscriberId, SurfaceState};
use duet_runtime::StoreHandle;
use duet_supervisor::{HostEvent, Supervisor, SurfaceAction, SurfaceId};

use crate::backend::WindowBackend;

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
    /// Does not tear its renderer down — call [`Host::tick`] or
    /// [`Host::request_suspend`] first if that is wanted.
    pub fn unregister(&mut self, id: SurfaceId) -> bool {
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
    /// [`HostEvent::Failed`].
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
    /// host reports [`HostEvent::Ready`] or [`HostEvent::Failed`] itself.
    /// Without that, a surface told to [`SurfaceAction::Start`] would sit in
    /// `Starting` forever and its memory would never be reclaimed.
    fn perform(&mut self, action: SurfaceAction, now: Instant) {
        let id = action.surface();
        let outcome = match action {
            SurfaceAction::Start(_) => self
                .backend
                .start_renderer(id)
                .and_then(|()| self.backend.attach_view(id)),
            SurfaceAction::Resume(_) => self.backend.attach_view(id),
            SurfaceAction::Suspend(_) => self.backend.detach_view(id),
            SurfaceAction::Teardown(_) => {
                // Drop subscriptions before destroying the renderer, so the
                // store cannot deliver to a surface that is going away.
                self.drop_subscriptions(id);
                self.backend.destroy_renderer(id)
            }
            // `SurfaceAction` is `#[non_exhaustive]`: every variant that
            // exists today is matched above, but the compiler requires this
            // arm so a future variant fails loudly here rather than being
            // silently treated as a no-op.
            _ => unreachable!(
                "duet-supervisor added a SurfaceAction variant duet-host does not handle yet"
            ),
        };

        match (action, outcome) {
            // Only a start needs confirming: Resume moves to Live immediately,
            // and Suspend/Teardown have already transitioned.
            (SurfaceAction::Start(_), Ok(())) => {
                self.supervisor.handle_at(now, HostEvent::Ready(id));
            }
            (_, Err(e)) => {
                self.supervisor
                    .handle_at(now, HostEvent::Failed(id, e.to_string()));
            }
            (_, Ok(())) => {}
        }
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
}
