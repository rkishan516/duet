//! The observable state store.

use crate::path::Path;
use crate::value::{SetError, Value};

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

/// One subscriber's registration at one path.
#[derive(Debug, Clone)]
struct Subscription {
    /// This subscription's own id, distinct from `subscriber`.
    id: SubscriptionId,
    /// The guest that owns this subscription.
    subscriber: SubscriberId,
    /// The path this subscription watches. [`Store::set`] notifies it
    /// whenever a write's path [`overlaps`](Path::overlaps) this one.
    path: Path,
}

/// A minimal change: the path that was written and its new value.
///
/// Every matching subscriber receives the same patch regardless of where
/// they subscribed — a subscriber watching root and a subscriber watching
/// the exact written path both get the identical `(path, value)` pair, not
/// a value re-rooted to their own subscription path. Clients apply it to a
/// local mirror keyed by `path`. This is what keeps a subscriber on a
/// 10,000-item list from receiving the whole list when one item changes: it
/// receives one `Patch` naming the one path that changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    /// The path that was written. Always the path passed to [`Store::set`],
    /// never the receiving subscriber's own subscribed path.
    pub path: Path,
    /// The value now at `path`, after the write.
    pub value: Value,
}

/// A patch addressed to one subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// The guest that should receive this notification.
    pub subscriber: SubscriberId,
    /// Which of that guest's subscriptions triggered it — a guest holding
    /// several overlapping subscriptions on one write receives one
    /// `Notification` per matching subscription, not one for the guest as a
    /// whole.
    pub subscription: SubscriptionId,
    /// What changed.
    pub patch: Patch,
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
        self.subscriptions.push(Subscription {
            id,
            subscriber,
            path,
        });
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

    /// Applies a write and returns the notifications it produced.
    ///
    /// A subscription is notified when its path
    /// [`overlaps`](Path::overlaps) `path`: a write to an ancestor of a
    /// subscription, to the subscription's own path, or to a descendant of
    /// it, all notify. Every matching notification's [`Patch`] carries
    /// `path` itself (the path written), not the receiving subscriber's own
    /// subscribed path — see [`Patch`]'s doc comment for why.
    ///
    /// Returning effects as data rather than invoking callbacks keeps this
    /// method pure enough to test directly, and lets the caller decide which
    /// thread each notification is delivered on — Phase 2's three-thread
    /// model needs exactly that freedom.
    ///
    /// **This method always notifies; it never diffs.** Every overlapping
    /// subscription is notified whether or not the value at `path` actually
    /// changed. Do not add an `if old != new` check as an optimisation:
    /// `Value` derives `PartialEq`, and IEEE 754 defines `NaN != NaN` (see
    /// the doc comment on [`Value::Float`]), so a tree containing a `NaN` is
    /// not equal to a clone of itself. A diffing `set` would therefore fire
    /// on every single write to such a subtree, forever, including genuine
    /// no-op writes — the exact case diffing exists to prevent. Always
    /// notifying makes that failure mode unreachable rather than merely
    /// unlikely.
    ///
    /// # Errors
    ///
    /// Returns whatever [`Value::set`] returns for the same `path` and
    /// `value`; see its documentation for the exact conditions. On error the
    /// tree is left completely unchanged (this is [`Value::set`]'s own
    /// guarantee) and no notifications are produced — the `Err` variant
    /// carries no `Vec`, so there is nothing to iterate or deliver.
    pub fn set(&mut self, path: &Path, value: Value) -> Result<Vec<Notification>, SetError> {
        self.root.set(path, value.clone())?;
        let patch = Patch {
            path: path.clone(),
            value,
        };
        Ok(self
            .subscriptions
            .iter()
            .filter(|s| s.path.overlaps(path))
            .map(|s| Notification {
                subscriber: s.subscriber,
                subscription: s.id,
                patch: patch.clone(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::path::{Path, Segment};
    use crate::value::Value;

    /// The fixture used across `store` tests.
    ///
    /// `documents` deliberately has 3 elements, not 1 — with a single
    /// element, a write that always touches "slot 0" is indistinguishable
    /// from a correct implementation that honours the requested index. This
    /// mirrors the same fix applied to `value`'s fixture (see its doc
    /// comment). `editor.zoom` and `editor.theme` keep their original values
    /// so the tests given directly in the task text, which reference them by
    /// path, are unaffected by this fixture's growth.
    fn sample() -> Value {
        Value::map([
            (
                "editor",
                Value::map([
                    ("zoom", Value::Float(1.0)),
                    ("theme", Value::Str("dark".into())),
                ]),
            ),
            (
                "documents",
                Value::List(vec![
                    Value::map([("title", Value::Str("first".into()))]),
                    Value::map([("title", Value::Str("second".into()))]),
                    Value::map([("title", Value::Str("third".into()))]),
                ]),
            ),
        ])
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

    // --- Task 8: writes produce notifications ---

    #[test]
    fn write_notifies_exact_path_subscriber() {
        let mut store = Store::new(sample());
        let (id, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscription, id);
        assert_eq!(notes[0].subscriber, SubscriberId(1));
        assert_eq!(notes[0].patch.path, p("editor.zoom"));
        assert_eq!(notes[0].patch.value, Value::Float(2.0));
    }

    #[test]
    fn write_notifies_ancestor_subscribers() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor"));
        store.subscribe(SubscriberId(2), Path::root());
        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn write_notifies_descendant_subscribers() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor.zoom"));
        // Replacing the whole `editor` struct may change `editor.zoom`.
        let notes = store
            .set(&p("editor"), Value::map([("zoom", Value::Float(9.0))]))
            .unwrap();
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn write_does_not_notify_siblings() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor.theme"));
        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn patch_carries_written_path_not_subscriber_path() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), Path::root());
        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        // A root subscriber receives the narrow patch, not the whole tree.
        assert_eq!(notes[0].patch.path, p("editor.zoom"));
        assert_eq!(notes[0].patch.value, Value::Float(2.0));
    }

    #[test]
    fn dropped_subscriber_stops_receiving_notifications() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), Path::root());
        store.subscribe(SubscriberId(2), Path::root());
        assert_eq!(store.drop_subscriber(SubscriberId(1)), 1);
        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscriber, SubscriberId(2));
    }

    // --- Required addition (A): the notification set is EXACTLY the set of
    // overlapping subscriptions, over a systematically enumerated corpus of
    // subscriptions, with the expected set computed independently of `set`'s
    // own filter so this test cannot pass by merely restating the
    // implementation. ---

    /// A tree with two levels of map nesting under three top-level keys
    /// (`a`, `b`, `c`), each holding three leaves (`a`, `b`, `c`), plus a
    /// sibling `d` outside that nest entirely — `d` exists so a write can
    /// land somewhere that overlaps none of the corpus subscriptions below,
    /// exercising the "notifies zero" case.
    fn many_paths_tree() -> Value {
        let branch = |base: i64| {
            Value::map([
                ("a", Value::Int(base)),
                ("b", Value::Int(base + 1)),
                ("c", Value::Int(base + 2)),
            ])
        };
        Value::map([
            ("a", branch(0)),
            ("b", branch(10)),
            ("c", branch(20)),
            ("d", Value::Int(99)),
        ])
    }

    /// Every `top.leaf` path for `top` and `leaf` each ranging over `a`, `b`,
    /// `c` — systematically enumerated by nested loop, not hand-listed one
    /// by one. Exactly 9 paths.
    fn leaf_corpus() -> Vec<Path> {
        const KEYS: [&str; 3] = ["a", "b", "c"];
        let mut corpus = Vec::new();
        for top in KEYS {
            for leaf in KEYS {
                corpus.push(Path::from_segments(vec![
                    Segment::Key(top.to_string()),
                    Segment::Key(leaf.to_string()),
                ]));
            }
        }
        assert_eq!(corpus.len(), 9, "corpus size changed; update deliberately");
        corpus
    }

    /// The 3 top-level branch paths `a`, `b`, `c` — systematically
    /// enumerated, not hand-listed. Unlike [`leaf_corpus`], subscribing here
    /// puts every subscription at an *ancestor* of the deeper write paths
    /// used below, which is exactly the direction [`leaf_corpus`] alone
    /// cannot exercise (see the doc comment on the test that uses this).
    fn branch_corpus() -> Vec<Path> {
        const KEYS: [&str; 3] = ["a", "b", "c"];
        let corpus: Vec<Path> = KEYS
            .iter()
            .map(|top| Path::from_segments(vec![Segment::Key(top.to_string())]))
            .collect();
        assert_eq!(corpus.len(), 3, "corpus size changed; update deliberately");
        corpus
    }

    /// Reimplements path overlap from scratch (rather than calling
    /// [`Path::overlaps`]) so the expected set in the property tests below
    /// is computed by genuinely independent logic, not merely a restatement
    /// of what `Store::set` already does internally.
    fn naive_overlaps(a: &Path, b: &Path) -> bool {
        fn is_prefix(a: &[Segment], b: &[Segment]) -> bool {
            a.len() <= b.len() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
        }
        is_prefix(a.segments(), b.segments()) || is_prefix(b.segments(), a.segments())
    }

    /// Subscribes one subscriber per path in `corpus` against a fresh store
    /// over [`many_paths_tree`], then for each `(write_path, new_value,
    /// expected_count)` case asserts that `Store::set`'s notified set
    /// exactly equals the set computed independently by filtering `corpus`
    /// with [`naive_overlaps`] — no extras, no omissions, exact count.
    fn assert_notifies_exactly(corpus: Vec<Path>, cases: &[(Path, Value, usize)]) {
        let mut store = Store::new(many_paths_tree());

        // One subscriber and one subscription per corpus path, plus the
        // path each was registered at, tracked independently of the store
        // so the expected set below never has to ask the store what it
        // thinks a subscription's path is.
        let registered: Vec<(SubscriptionId, SubscriberId, Path)> = corpus
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let subscriber = SubscriberId(i as u64);
                let (id, _snapshot) = store.subscribe(subscriber, path.clone());
                (id, subscriber, path.clone())
            })
            .collect();

        for (write_path, new_value, expected_count) in cases {
            let expected: BTreeSet<(u64, u64)> = registered
                .iter()
                .filter(|(_, _, path)| naive_overlaps(path, write_path))
                .map(|(sub_id, subscriber, _)| (sub_id.0, subscriber.0))
                .collect();
            assert_eq!(
                expected.len(),
                *expected_count,
                "expected-set computation itself drifted for write {write_path}"
            );

            let notes = store.set(write_path, new_value.clone()).unwrap();
            let actual: BTreeSet<(u64, u64)> = notes
                .iter()
                .map(|n| (n.subscription.0, n.subscriber.0))
                .collect();

            assert_eq!(
                notes.len(),
                *expected_count,
                "count mismatch for write {write_path}"
            );
            assert_eq!(
                actual, expected,
                "notified set mismatch for write {write_path}"
            );
        }
    }

    #[test]
    fn notification_set_exactly_matches_overlapping_subscriptions() {
        // Subscriptions at every leaf (depth 2). Write paths chosen to hit
        // zero, one, some, and all of the 9 registered subscriptions.
        let cases: [(Path, Value, usize); 4] = [
            (p("d"), Value::Int(-1), 0),
            (p("a.a"), Value::Int(-1), 1),
            (p("a"), Value::Int(-1), 3),
            (Path::root(), Value::Int(-1), 9),
        ];
        assert_notifies_exactly(leaf_corpus(), &cases);
    }

    /// Companion to the leaf-only property test above. Every subscription
    /// there sits at or below every write path used, so it can never
    /// exercise the direction where the *subscriber* is an ancestor of the
    /// *write* (e.g. subscribed at `editor`, written at `editor.zoom`) —
    /// a `set` that only notifies descendant-of-write subscribers would
    /// still pass it. Subscribing at the 3 branches instead of the 9 leaves
    /// puts every subscription strictly above the leaf write path used
    /// here, closing that gap: this test alone is enough to catch that
    /// direction being dropped.
    #[test]
    fn notification_set_exactly_matches_overlapping_subscriptions_with_ancestor_subscribers() {
        let cases: [(Path, Value, usize); 4] = [
            (p("d"), Value::Int(-1), 0),
            (p("a.a"), Value::Int(-1), 1), // subscriber `a` is an ancestor of write `a.a`.
            (p("a"), Value::Int(-1), 1),
            (Path::root(), Value::Int(-1), 3),
        ];
        assert_notifies_exactly(branch_corpus(), &cases);
    }

    // --- Required addition (B): a failed write is a genuine frame condition
    // — the whole tree is checked, not one hand-picked path — and, since
    // there is no `Vec` to inspect on `Err` by construction, the registry's
    // integrity is proven by checking a subsequent successful write still
    // notifies exactly the right subscribers. ---

    #[test]
    fn failed_write_leaves_tree_and_registry_intact() {
        let failing_paths: [Path; 5] = [
            p("nope.deeper"),
            p("editor.zoom.deeper"),
            p("documents[9]"),
            p("editor[0]"),
            p("documents.title"),
        ];

        for failing_path in failing_paths {
            let mut store = Store::new(sample());
            let (zoom_sub, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));
            let (root_sub, _) = store.subscribe(SubscriberId(2), Path::root());

            let result = store.set(&failing_path, Value::Null);
            assert!(
                result.is_err(),
                "expected {failing_path} to fail so the untouched-state guarantee is exercised"
            );

            // (1) The entire tree, not just one field, is unchanged.
            assert_eq!(
                store.get(&Path::root()),
                Some(&sample()),
                "tree mutated after failed write at {failing_path}"
            );

            // (2) No notifications could have been produced: `set` returns
            // `Result<Vec<Notification>, SetError>`, and the `Err` branch
            // carries no `Vec` at all, so there is nothing to iterate or
            // deliver. That is a property of the type, not something this
            // test needs to inspect further.

            // (3) The subscription registry itself was not corrupted by the
            // failed attempt: a subsequent successful write still notifies
            // exactly the two subscribers registered above, by their
            // original subscription ids.
            let notes = store.set(&p("editor.zoom"), Value::Float(9.0)).unwrap();
            let mut actual: Vec<u64> = notes.iter().map(|n| n.subscription.0).collect();
            actual.sort_unstable();
            let mut expected = vec![zoom_sub.0, root_sub.0];
            expected.sort_unstable();
            assert_eq!(
                actual, expected,
                "registry corrupted after failed write at {failing_path}"
            );
        }
    }

    // --- Required addition (C): subscription registry edge cases ---

    #[test]
    fn one_subscriber_with_multiple_subscriptions_gets_multiple_notifications() {
        let mut store = Store::new(sample());
        let (sub_zoom, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        let (sub_root, _) = store.subscribe(SubscriberId(1), Path::root());

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|n| n.subscriber == SubscriberId(1)));
        let mut actual: Vec<u64> = notes.iter().map(|n| n.subscription.0).collect();
        actual.sort_unstable();
        let mut expected = vec![sub_zoom.0, sub_root.0];
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn two_different_subscribers_on_same_path_are_both_notified() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor.zoom"));
        store.subscribe(SubscriberId(2), p("editor.zoom"));

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        assert_eq!(notes.len(), 2);
        let mut subscribers: Vec<u64> = notes.iter().map(|n| n.subscriber.0).collect();
        subscribers.sort_unstable();
        assert_eq!(subscribers, vec![1, 2]);
    }

    #[test]
    fn same_subscriber_subscribing_twice_to_same_path_gets_two_distinct_notifications() {
        let mut store = Store::new(sample());
        let (id_a, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        let (id_b, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        assert_ne!(id_a, id_b);

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        assert_eq!(notes.len(), 2);
        let mut actual: Vec<u64> = notes.iter().map(|n| n.subscription.0).collect();
        actual.sort_unstable();
        let mut expected = vec![id_a.0, id_b.0];
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn drop_subscriber_with_no_subscriptions_returns_zero_and_disturbs_nothing() {
        let mut store = Store::new(sample());
        let (root_sub, _) = store.subscribe(SubscriberId(1), Path::root());

        assert_eq!(store.drop_subscriber(SubscriberId(99)), 0);

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscription, root_sub);
        assert_eq!(notes[0].subscriber, SubscriberId(1));
    }

    #[test]
    fn unsubscribe_with_id_from_a_different_store_returns_false_and_disturbs_nothing() {
        // `SubscriptionId`s are only unique within one `Store` (documented on
        // the type), so two fresh stores both mint id 0 first. To get an id
        // that genuinely does not exist in `store_a`, churn `store_b`'s
        // counter forward first and take an id past anything `store_a` ever
        // hands out.
        let mut store_a = Store::new(sample());
        let mut store_b = Store::new(sample());
        store_b.subscribe(SubscriberId(9), Path::root());
        store_b.subscribe(SubscriberId(9), Path::root());
        let (id_from_b, _) = store_b.subscribe(SubscriberId(1), Path::root());
        let (id_in_a, _) = store_a.subscribe(SubscriberId(2), Path::root());
        assert_ne!(
            id_from_b, id_in_a,
            "test premise requires these ids to differ"
        );

        assert!(!store_a.unsubscribe(&id_from_b));

        // `store_a`'s own subscription must still be intact.
        let notes = store_a.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscription, id_in_a);
    }

    #[test]
    fn subscription_ids_are_never_reused_after_unsubscribe() {
        let mut store = Store::new(sample());
        let (id1, _) = store.subscribe(SubscriberId(1), Path::root());
        store.unsubscribe(&id1);
        let (id2, _) = store.subscribe(SubscriberId(1), Path::root());
        assert_ne!(id1, id2);
    }

    #[test]
    fn subscribe_to_not_yet_existing_path_is_notified_once_a_write_creates_it() {
        let mut store = Store::new(sample());
        let (id, snapshot) = store.subscribe(SubscriberId(1), p("editor.wrap"));
        assert_eq!(snapshot, None);

        let notes = store.set(&p("editor.wrap"), Value::Bool(true)).unwrap();

        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscription, id);
        assert_eq!(notes[0].patch.value, Value::Bool(true));
        assert_eq!(store.get(&p("editor.wrap")), Some(&Value::Bool(true)));
    }

    // --- Required addition (D): snapshot semantics ---

    #[test]
    fn subscribe_snapshot_is_a_deep_copy_independent_of_later_writes() {
        let mut store = Store::new(sample());
        let (_id, snapshot) = store.subscribe(SubscriberId(1), p("editor"));

        store.set(&p("editor.zoom"), Value::Float(99.0)).unwrap();

        assert_eq!(
            snapshot,
            Some(Value::map([
                ("zoom", Value::Float(1.0)),
                ("theme", Value::Str("dark".into())),
            ])),
            "snapshot taken at subscribe time must not observe a later write"
        );
    }

    #[test]
    fn subscribing_at_root_returns_the_whole_tree() {
        let mut store = Store::new(sample());
        let (_id, snapshot) = store.subscribe(SubscriberId(1), Path::root());
        assert_eq!(snapshot, Some(sample()));
    }
}
