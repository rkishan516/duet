//! The observable state store.

use crate::path::Path;
use crate::value::Value;

/// Identifies a guest that holds subscriptions, e.g. a Flutter or webview
/// surface.
///
/// A guest may hold many subscriptions at once (see [`SubscriptionId`]); this
/// id groups them so [`Store::drop_subscriber`] can remove them all at once
/// when a surface goes cold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberId(pub u64);

/// Identifies one subscription held by a subscriber.
///
/// Distinct from [`SubscriberId`] because one subscriber may hold several
/// subscriptions at different paths, each independently created and removed.
/// Ids are minted by [`Store::subscribe`] in increasing order and are never
/// reused, even after [`Store::unsubscribe`] removes the subscription that
/// held one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub u64);

/// One subscriber's registration.
///
/// The path a subscription watches is not stored here yet: nothing in this
/// commit reads it back (the snapshot is computed once, at subscribe time,
/// and discarded). It is added in the commit that introduces
/// [`Store::set`], which needs it to decide who to notify.
#[derive(Debug, Clone)]
struct Subscription {
    /// This subscription's own id, distinct from `subscriber`.
    id: SubscriptionId,
    /// The guest that owns this subscription.
    subscriber: SubscriberId,
}

/// The authoritative state tree plus its subscription registry.
///
/// `Store` owns the single source of truth for application state. Guest
/// processes (Flutter, the Tauri webview) never hold state themselves — they
/// subscribe to paths here and mirror what they are told. This is what lets
/// either renderer be torn down and recreated without losing state: the
/// guest simply resubscribes and receives a fresh snapshot (see
/// [`Store::subscribe`]).
#[derive(Debug, Clone)]
pub struct Store {
    /// The state tree itself.
    root: Value,
    /// Every live subscription, across all subscribers.
    subscriptions: Vec<Subscription>,
    /// The next id [`Store::subscribe`] will mint. Monotonically increasing
    /// so that ids are never reused within a `Store`'s lifetime, even after
    /// removals.
    next_id: u64,
}

impl Store {
    /// Creates a store seeded with `root` and no subscriptions.
    pub fn new(root: Value) -> Self {
        Store {
            root,
            subscriptions: Vec::new(),
            next_id: 0,
        }
    }

    /// Reads the value at `path`, or `None` if it does not exist.
    ///
    /// Delegates directly to [`Value::get`]; see its documentation for
    /// exactly which conditions produce `None`.
    pub fn get(&self, path: &Path) -> Option<&Value> {
        self.root.get(path)
    }

    /// Registers a subscription at `path` for `subscriber` and returns its
    /// id plus the current value at `path`, if any.
    ///
    /// The snapshot is why resuming from a cold surface needs no special
    /// path: a guest that restarts simply subscribes again and receives
    /// current state, rather than requiring the host to replay history. The
    /// returned `Value` is an independent clone; later writes to the store
    /// never change a snapshot already handed out.
    ///
    /// Subscribing to a path that does not currently exist is legal and
    /// returns `None` — the subscription is still registered, and a later
    /// write that creates the path will notify it normally.
    pub fn subscribe(
        &mut self,
        subscriber: SubscriberId,
        path: Path,
    ) -> (SubscriptionId, Option<Value>) {
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        let snapshot = self.root.get(&path).cloned();
        self.subscriptions.push(Subscription { id, subscriber });
        (id, snapshot)
    }

    /// Removes one subscription by id.
    ///
    /// Returns `true` if a subscription with this id was present and
    /// removed, `false` if no such subscription existed (already removed,
    /// or an id from a different `Store`).
    pub fn unsubscribe(&mut self, id: &SubscriptionId) -> bool {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| &s.id != id);
        self.subscriptions.len() != before
    }

    /// Removes every subscription held by `subscriber`, regardless of path.
    ///
    /// Called when a surface goes cold and all of its subscriptions become
    /// meaningless at once. Returns how many subscriptions were removed;
    /// `0` if `subscriber` held none.
    pub fn drop_subscriber(&mut self, subscriber: SubscriberId) -> usize {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| s.subscriber != subscriber);
        before - self.subscriptions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;
    use crate::value::Value;

    fn sample() -> Value {
        Value::map([(
            "editor",
            Value::map([
                ("zoom", Value::Float(1.0)),
                ("theme", Value::Str("dark".into())),
            ]),
        )])
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn subscribe_returns_snapshot_of_that_path() {
        let mut store = Store::new(sample());
        let (_id, snapshot) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        assert_eq!(snapshot, Some(Value::Float(1.0)));
    }

    #[test]
    fn subscribe_to_missing_path_returns_no_snapshot() {
        let mut store = Store::new(sample());
        let (_id, snapshot) = store.subscribe(SubscriberId(1), p("editor.nope"));
        assert_eq!(snapshot, None);
    }

    #[test]
    fn subscription_ids_are_unique() {
        let mut store = Store::new(sample());
        let (a, _) = store.subscribe(SubscriberId(1), Path::root());
        let (b, _) = store.subscribe(SubscriberId(1), Path::root());
        assert_ne!(a, b);
    }

    #[test]
    fn unsubscribe_removes_the_subscription() {
        let mut store = Store::new(sample());
        let (id, _) = store.subscribe(SubscriberId(1), Path::root());
        assert!(store.unsubscribe(&id));
        assert!(
            !store.unsubscribe(&id),
            "second removal should report false"
        );
    }

    #[test]
    fn get_reads_through_to_the_tree() {
        let store = Store::new(sample());
        assert_eq!(
            store.get(&p("editor.theme")),
            Some(&Value::Str("dark".into()))
        );
    }
}
