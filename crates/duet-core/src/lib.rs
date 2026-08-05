//! Platform-free core of the Duet framework.
//!
//! # The problem
//!
//! Duet runs a Rust host process alongside two renderers in separate
//! windows: a Flutter engine and a Tauri webview. Either renderer can be
//! torn down at any moment to reclaim memory — a minimized window, an
//! idle timeout, or an explicit close can all end a renderer's process.
//! Because either side can vanish and come back, **neither renderer is
//! allowed to own state**. If a Flutter widget or a JavaScript closure held
//! the authoritative copy of some value, tearing down that renderer would
//! lose it.
//!
//! # The governing principle
//!
//! **State survives teardown. Events don't.**
//!
//! If a value must outlive a suspended renderer, it lives in the [`store`] —
//! never in a renderer's own memory. Events (notifications about a write)
//! are delivered only to surfaces that are currently live; they are never
//! queued for a surface that is cold, and never replayed later. A surface
//! that comes back simply asks the store what is there now.
//!
//! # The store and the overlap rule
//!
//! [`store::Store`] is the single most important concept here: an
//! authoritative [`value::Value`] tree plus a registry of subscriptions, each
//! watching one [`path::Path`]. A subscriber at path `P` is notified about a
//! write at path `W` exactly when `P` and `W` [overlap](path::Path::overlaps)
//! — when either is a prefix of the other (see [`path`] for the precise
//! definition, including that this comparison is by path *segment*, not by
//! string prefix).
//!
//! Concretely: writing `editor.zoom` notifies subscribers at `editor.zoom`
//! (exact match), `editor` (an ancestor — its value may have changed because
//! one of its fields did), and the root (the whole tree is every path's
//! ancestor) — but never a subscriber at `editor.theme`, a sibling that this
//! write cannot affect.
//!
//! # Patches carry the written path
//!
//! A [`store::Notification`]'s [`store::Patch`] always carries the path that
//! was *written*, never the receiving subscriber's own subscribed path. A
//! subscriber watching root and a subscriber watching the exact path that
//! changed both receive the identical, narrow patch. This is why a
//! subscriber on a 10,000-item list is not sent the whole list every time one
//! item changes: it receives one `Patch` naming the one path that changed,
//! regardless of where in the tree it happened to subscribe.
//!
//! # The lifecycle
//!
//! [`lifecycle::SurfaceState`] tracks where one renderer sits: `Cold`
//! (nothing running), `Starting` (booting), `Live` (attached and rendering),
//! `Suspending` (a grace period before teardown), and `Failed` (crashed, with
//! a reason published back into the store so the *other* surface can render
//! an error UI). `Suspending` exists specifically to prevent thrash: closing
//! and immediately reopening a window returns straight to `Live` from
//! `Suspending`, skipping a full engine boot, rather than tearing down and
//! rebuilding on every quick close/reopen.
//!
//! # Effects as data
//!
//! [`store::Store::set`] returns the notifications a write produced as a
//! plain `Vec`, rather than invoking callbacks itself. This keeps the store
//! pure enough to test directly, and lets the caller — the host's event loop
//! — decide which thread each notification is actually delivered on. Phase
//! 2's three-thread model depends on exactly this freedom.
//!
//! # Module map
//!
//! - [`path`] — addressing into the state tree (`editor.zoom`,
//!   `documents[3].title`) and the overlap rule that drives subscriptions.
//! - [`value`] — the dynamic, path-addressable state tree itself.
//! - [`store`] — subscriptions, snapshots, and write notifications.
//! - [`lifecycle`] — the per-surface state machine (`Cold` through `Failed`).
//! - [`policy`] — pure decisions about when a live surface should suspend or
//!   tear down.
//!
//! # A full cycle, executable
//!
//! ```
//! use duet_core::{Path, SubscriberId, Store, Value};
//!
//! // Create a store and subscribe to a path.
//! let mut store = Store::new(Value::map([("counter", Value::Int(0))]));
//! let surface = SubscriberId(1);
//! let path = Path::parse("counter").unwrap();
//! let (subscription, snapshot) = store.subscribe(surface, path.clone());
//! assert_eq!(snapshot, Some(Value::Int(0)));
//!
//! // A write while the subscriber is live produces a notification.
//! let notes = store.set(&path, Value::Int(1)).unwrap();
//! assert_eq!(notes.len(), 1);
//! assert_eq!(notes[0].subscription, subscription);
//! assert_eq!(notes[0].patch.value, Value::Int(1));
//!
//! // Teardown: the surface goes cold, dropping its subscriptions.
//! store.drop_subscriber(surface);
//!
//! // A write while cold produces no notification for anyone -- events
//! // don't survive teardown.
//! let notes = store.set(&path, Value::Int(2)).unwrap();
//! assert!(notes.is_empty());
//!
//! // But the write itself is durable: resubscribing after "resume" sees
//! // both writes made while the surface was gone, not just the last one.
//! let (_subscription, snapshot) = store.subscribe(surface, path);
//! assert_eq!(snapshot, Some(Value::Int(2)));
//! ```

#![deny(missing_docs)]

mod echo;
pub mod lifecycle;
pub mod path;
pub mod policy;
pub mod store;
pub mod value;

pub use lifecycle::{Instant, InvalidTransition, LifecycleEvent, SurfaceState, transition};
pub use path::{Path, PathParseError, Segment};
pub use policy::{Decision, Policy, PolicyInput, evaluate};
pub use store::{Notification, Patch, Store, SubscriberId, SubscriptionId};
pub use value::{SetError, Value};

/// Phase 2 moves `Store` onto a dedicated core thread and marshals notifications
/// to the main thread, so these bounds are load-bearing. Asserted here so a
/// change that breaks them fails at its own source rather than at the
/// integration point. In particular, switching `Patch` to `Rc` instead of `Arc`
/// would trip this.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<store::Store>();
    assert_send_sync::<store::Notification>();
    assert_send_sync::<value::Value>();
    assert_send_sync::<path::Path>();
};
