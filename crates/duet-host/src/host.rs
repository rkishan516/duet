//! Orchestrates the supervisor, the store and a platform backend.

use std::collections::BTreeMap;

use duet_core::{Policy, SubscriberId, SurfaceState};
use duet_runtime::StoreHandle;
use duet_supervisor::{Supervisor, SurfaceId};

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
    /// Does not tear its renderer down — call `Host::tick` or
    /// `Host::request_suspend` first if that is wanted. Neither exists yet;
    /// both land in Phase 2b-2 Task 3.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::RecordingBackend;
    use duet_core::{Policy, Value};
    use duet_runtime::{NullSink, Runtime};

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
}
