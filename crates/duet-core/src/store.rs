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
    ///
    /// A flat `Vec` scanned linearly on every [`Store::set`]. This is
    /// deliberate for now, not an oversight: it is cache-friendly, it is
    /// what makes [`Store::unsubscribe`] and [`Store::drop_subscriber`]
    /// one-line `retain` calls, and a linear scan needs thousands of live
    /// subscriptions before an index would win. When it does need to
    /// change, the planned shape keeps root subscriptions in their own
    /// always-notified list (since root overlaps every write) and buckets
    /// the rest by their path's first segment, so `set` only scans the
    /// bucket a write's first segment can possibly touch.
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
    /// The order of notifications in the returned `Vec` is **unspecified**
    /// and callers must not depend on it. It happens to follow subscribe
    /// order today (it is derived from a linear scan of `subscriptions`),
    /// but that is an implementation detail: Phase 2 adds threading, and the
    /// bucketed-by-first-segment index described on the `subscriptions`
    /// field would also reorder it. Ordering *within* one `set` call is
    /// semantically meaningless anyway, since every notification from a
    /// single call carries an identical [`Patch`] to a different
    /// subscription — whatever causality matters lives between successive
    /// `set` calls, not inside this `Vec`.
    ///
    /// `path` is committed to the tree before notifications are derived,
    /// and every derived [`Patch::value`] is cloned from the caller's
    /// `value` — never read back from `self.root` after the write. Together
    /// these two facts are why the relative order of "mutate the tree" and
    /// "build the notification list" inside this method is not currently
    /// observable from outside it: nothing here depends on post-write tree
    /// state. That stops being true the moment a future change needs to
    /// filter or shape a notification using the *post-write* tree (for
    /// example, a write that shortens a list, invalidating subscriptions at
    /// now-out-of-bounds indices) — at that point the order genuinely
    /// matters and this note is what should catch it at review time.
    ///
    /// Each [`Notification`] currently clones its [`Patch`] once per
    /// matching subscription. Sharing a single allocation (e.g.
    /// `Arc<Patch>`) instead would remove that duplication, but the choice
    /// between `Rc` and `Arc` is itself a threading decision, and Phase 1
    /// deliberately has no threading model yet — that choice belongs to
    /// whichever Phase 2 change first needs notifications to cross a thread
    /// boundary, not here.
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
    /// comment).
    ///
    /// `editor` and `matrix`'s maps/lists stay at 2-3 entries rather than
    /// growing to match. The crate's ">= 3 elements" bar exists to defeat
    /// "always touches slot 0" bugs, which is specifically an *index*
    /// hazard — `editor`'s two entries are addressed by name (`zoom`,
    /// `theme`), and a bug that always reads/writes "the first key
    /// regardless of which key was requested" is exactly as detectable with
    /// 2 named keys as with 30, so growing it buys nothing here. `matrix`
    /// exists to represent list-in-list nesting (a list is itself a valid
    /// list element), which none of the other fields cover.
    ///
    /// `editor.zoom`, `editor.theme`, and `documents[i].title` keep their
    /// original values and shapes so the tests given directly in the task
    /// text, which reference them by path, are unaffected by this fixture's
    /// growth.
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
            (
                "matrix",
                Value::List(vec![
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                    Value::List(vec![Value::Int(3)]),
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

    /// A tree exercising the two path-segment boundary cases neither
    /// [`many_paths_tree`] corpus contains: distinct list indices at the
    /// same depth, and map keys that are string-prefixes of one another.
    /// `documents` mirrors [`sample`]'s 3-element list (not fewer, for the
    /// same "slot 0" reason documented there); `edit`/`editor` are a
    /// deliberate string-prefix pair.
    fn boundary_tree() -> Value {
        Value::map([
            (
                "documents",
                Value::List(vec![
                    Value::map([("title", Value::Str("first".into()))]),
                    Value::map([("title", Value::Str("second".into()))]),
                    Value::map([("title", Value::Str("third".into()))]),
                ]),
            ),
            ("edit", Value::Int(1)),
            ("editor", Value::Int(2)),
        ])
    }

    /// Four deliberately hand-picked boundary paths, unlike [`leaf_corpus`]
    /// and [`branch_corpus`]'s systematic sweeps: each pair here targets one
    /// specific confusion a broken overlap check could make.
    fn boundary_corpus() -> Vec<Path> {
        let corpus = vec![
            p("documents[0].title"),
            p("documents[1]"),
            p("edit"),
            p("editor"),
        ];
        assert_eq!(corpus.len(), 4, "corpus size changed; update deliberately");
        corpus
    }

    /// Every prefix of `path`, including the root and the path itself,
    /// rendered as strings for set membership.
    ///
    /// Deliberately a different formulation from `Path::overlaps`: two
    /// paths overlap exactly when their prefix closures intersect.
    /// Computing the expectation the same way the implementation does
    /// (a length-guarded zip-and-compare, run in each direction) would make
    /// this test a restatement rather than a check — which is exactly what
    /// happened here in an earlier draft: a zip-and-compare "independent"
    /// oracle with renamed bindings still encoded the identical concept of
    /// overlap, so a wrong *concept* of overlap in the implementation would
    /// have been mirrored identically in the oracle and passed anyway. Set
    /// intersection of prefix closures is a genuinely different formulation
    /// of the same relation.
    fn prefix_closure(path: &Path) -> BTreeSet<String> {
        let segs = path.segments();
        (0..=segs.len())
            .map(|n| Path::from_segments(segs[..n].to_vec()).to_string())
            .collect()
    }

    /// The expected-set oracle used by the property tests below.
    ///
    /// `a` overlaps `b` exactly when `a` is an ancestor-or-equal of `b`, or
    /// `b` is an ancestor-or-equal of `a`. Phrased over prefix closures: `a`
    /// is an ancestor-or-equal of `b` exactly when `a`'s own rendered path
    /// appears in `b`'s closure (every prefix of `b`, including `b` itself
    /// and root). That membership check — not closure *intersection* — is
    /// the correct translation. Checking whether the two closures share any
    /// member at all is a different and strictly weaker claim: root belongs
    /// to every path's closure, so any two closures always intersect there
    /// regardless of whether the two paths are related at all.
    ///
    /// An earlier draft of this function did check intersection
    /// (`!prefix_closure(a).is_disjoint(&prefix_closure(b))`) and
    /// consequently treated every pair of paths in the corpus as
    /// overlapping — caught immediately by
    /// [`oracle_overlaps_agrees_with_path_overlaps_on_the_corpus`] below,
    /// which is exactly the cross-check that test exists to provide: this
    /// oracle backs every property test's expected set, so an oracle bug
    /// would otherwise have silently made every one of them vacuous.
    fn oracle_overlaps(a: &Path, b: &Path) -> bool {
        prefix_closure(b).contains(&a.to_string()) || prefix_closure(a).contains(&b.to_string())
    }

    /// Cross-checks [`oracle_overlaps`] against the real [`Path::overlaps`]
    /// over every pair drawn from all four corpora used below, plus root.
    /// The property tests only ever compare `Store::set`'s output against
    /// the oracle, never against `Path::overlaps` directly, so this is what
    /// actually stands behind "the oracle is trustworthy" rather than that
    /// being an unverified assumption.
    #[test]
    fn oracle_overlaps_agrees_with_path_overlaps_on_the_corpus() {
        let mut all_paths = leaf_corpus();
        all_paths.extend(branch_corpus());
        all_paths.extend(boundary_corpus());
        all_paths.push(Path::root());

        for a in &all_paths {
            for b in &all_paths {
                assert_eq!(
                    oracle_overlaps(a, b),
                    a.overlaps(b),
                    "oracle disagrees with Path::overlaps for {a} vs {b}"
                );
            }
        }
    }

    /// Subscribes one subscriber per path in `corpus` against a fresh store
    /// over `tree`, then for each `(write_path, new_value, expected_count)`
    /// case asserts that `Store::set`'s notified set exactly equals the set
    /// computed independently by filtering `corpus` with
    /// [`oracle_overlaps`] — no extras, no omissions, exact count.
    fn assert_notifies_exactly(tree: Value, corpus: Vec<Path>, cases: &[(Path, Value, usize)]) {
        let mut store = Store::new(tree);

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
                .filter(|(_, _, path)| oracle_overlaps(path, write_path))
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
        assert_notifies_exactly(many_paths_tree(), leaf_corpus(), &cases);
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
        assert_notifies_exactly(many_paths_tree(), branch_corpus(), &cases);
    }

    /// Neither [`leaf_corpus`] nor [`branch_corpus`] alone ever puts an
    /// ancestor *and* a descendant of the same write in one registry at
    /// once — the leaf corpus write-to-branch case has no ancestor
    /// subscriber, and the branch corpus write-to-leaf case has no
    /// descendant subscriber. Combining both corpora into one 12-entry
    /// registry closes that completeness gap: a write inside `a` now hits
    /// both the branch subscriber `a` and its leaf subscribers at once.
    #[test]
    fn notification_set_exactly_matches_overlapping_subscriptions_with_combined_corpus() {
        let corpus: Vec<Path> = [branch_corpus(), leaf_corpus()].concat();
        assert_eq!(corpus.len(), 12, "combined corpus size changed");

        let cases: [(Path, Value, usize); 4] = [
            (p("d"), Value::Int(-1), 0),
            // `a` (ancestor) and `a.a` (exact) both match; `a.b`/`a.c` do not.
            (p("a.a"), Value::Int(-1), 2),
            // `a` itself, plus its 3 leaves `a.a`, `a.b`, `a.c`.
            (p("a"), Value::Int(-1), 4),
            (Path::root(), Value::Int(-1), 12),
        ];
        assert_notifies_exactly(many_paths_tree(), corpus, &cases);
    }

    /// Neither [`many_paths_tree`]'s corpora contain a list index or a
    /// string-prefix key pair, so a `Store::set` that (a) treated every
    /// `Segment::Index` as equal to every other, or (b) computed overlap by
    /// comparing `Path`'s *rendered string* rather than its segments, could
    /// pass every property test above and still be wrong: `path.rs` pins
    /// both directly (so the composed system is safe today), but nothing in
    /// `store`'s own tests would catch either regression locally.
    #[test]
    fn notification_set_respects_index_and_string_prefix_boundaries() {
        // Cases run in sequence against one shared store (see
        // `assert_notifies_exactly`), and each write *replaces* whatever is
        // at its path. So the ancestor write to `documents[1].title` must
        // run before the exact write to `documents[1]` replaces that slot
        // with a scalar — otherwise the later write would try to insert a
        // key into a scalar and fail. Which subscriptions a write notifies
        // depends only on paths, never on the values in the tree, so this
        // reordering does not change any of the expected counts below.
        let cases: [(Path, Value, usize); 5] = [
            // Neither `documents[0].title` nor `documents[1]` overlaps a
            // write to a sibling index.
            (p("documents[2]"), Value::Int(-1), 0),
            // Same-index ancestor write: `documents[1]` (ancestor) matches;
            // `documents[0].title` (different index) does not. This is the
            // positive control paired with the negative case below — it
            // proves the ancestor direction still works with indices, so
            // that case's zero-for-index-0 result isn't just "the filter
            // always returns false".
            (p("documents[1].title"), Value::Int(-1), 1),
            // Exact index match: `documents[1]` matches itself, but
            // `documents[0].title` must not — a mutant collapsing all
            // indices together would notify both.
            (p("documents[1]"), Value::Int(-1), 1),
            // `editor` is not a string-prefix confusion of `edit` (exact
            // `Segment::Key` equality, not `str::starts_with`).
            (p("editor"), Value::Int(-1), 1),
            (p("edit"), Value::Int(-1), 1),
        ];
        assert_notifies_exactly(boundary_tree(), boundary_corpus(), &cases);
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

        // Constructed once, outside the loop, so every failing path below
        // runs against the *same* store in sequence rather than each
        // getting a fresh one. A registry-corruption bug that only
        // manifests on the second (or fifth) consecutive failure — as
        // opposed to any single isolated failure — would be invisible if
        // each iteration reset the store.
        let mut store = Store::new(sample());
        let (zoom_sub, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        let (root_sub, _) = store.subscribe(SubscriberId(2), Path::root());

        for failing_path in &failing_paths {
            let result = store.set(failing_path, Value::Null);
            assert!(
                result.is_err(),
                "expected {failing_path} to fail so the untouched-state guarantee is exercised"
            );

            // (1) The entire tree, not just one field, is unchanged —
            // checked after every failure in the sequence, not only after
            // the last one.
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
        }

        // (3) The subscription registry itself was not corrupted by that
        // whole run of consecutive failed attempts: a subsequent successful
        // write still notifies exactly the two subscribers registered
        // above, by their original subscription ids.
        let notes = store.set(&p("editor.zoom"), Value::Float(9.0)).unwrap();
        let mut actual: Vec<u64> = notes.iter().map(|n| n.subscription.0).collect();
        actual.sort_unstable();
        let mut expected = vec![zoom_sub.0, root_sub.0];
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "registry corrupted after a run of consecutive failed writes"
        );
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
