//! Threading runtime for Duet.
//!
//! [`duet_core::Store`] is deliberately single-threaded plain data. This crate
//! moves it onto a dedicated **core thread** and hands out cheap, cloneable
//! [`StoreHandle`]s that any thread may use.
//!
//! # Why a thread rather than a mutex
//!
//! A write returns its effects as data — `Vec<Notification>` — which must be
//! delivered somewhere. Under a mutex, the writer would have to deliver either
//! while holding the lock (stalling every other writer) or after releasing it
//! (allowing two writes' notifications to be reordered relative to the writes).
//! One owning thread makes write order and notification order the same order.
//!
//! The main thread also runs the tao event loop, Flutter's platform thread and
//! the webview, so it must never block on store work.
//!
//! # Delivery
//!
//! Notifications are handed to a [`Sink`]. That is a trait rather than `tao`'s
//! `EventLoopProxy` so this crate stays testable without a window system;
//! Phase 2b supplies the real implementation.
//!
//! The most surprising fact about this crate: [`Sink::deliver`] runs
//! synchronously **on the core thread**, before it accepts the next request.
//! See [`Sink::deliver`]'s docs for exactly what that constrains.
//!
//! # Not yet implemented
//!
//! **Buffering across the `Starting` gap.** [`duet_core::Store::subscribe`]
//! makes it the caller's obligation to buffer notifications that arrive
//! between a subscriber's snapshot and its surface becoming ready, rather
//! than discarding them. `duet-runtime` does not yet do this. When it lands
//! it belongs **here**, as a [`Sink`] adapter that holds batches per
//! subscriber and flushes on transition to `Live` — not in the platform
//! layer, where it could not be tested without a window system.
//!
//! **A machine-readable error code.** [`RuntimeError`] and [`SinkError`] are
//! designed for human-readable `Display` today. A `fn code(&self) ->
//! &'static str` (or similar) is deferred until Phase 2b's wire codec exists,
//! so the shape can be designed against a real format instead of guessed at.
//!
//! # A full cycle, executable
//!
//! ```
//! use duet_core::{Path, Value};
//! use duet_runtime::{RecordingSink, Runtime};
//!
//! // Spawn a runtime with a sink that records what it receives.
//! let sink = RecordingSink::new();
//! let root = Value::map([("counter", Value::Int(0))]);
//! let runtime = Runtime::spawn(root, sink.clone());
//! let handle = runtime.handle();
//!
//! // Never invent a SubscriberId -- a collision silently cross-delivers
//! // notifications across a trust boundary. Always allocate one.
//! let surface = handle.next_subscriber_id();
//! let path = Path::parse("counter").unwrap();
//! let (_subscription, snapshot) = handle.subscribe(surface, path.clone()).unwrap();
//! assert_eq!(snapshot, Some(Value::Int(0)));
//!
//! // A write is delivered to the sink as one notification batch.
//! handle.set(&path, Value::Int(1)).unwrap();
//! runtime.shutdown().unwrap();
//! assert_eq!(sink.notifications().len(), 1);
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod command;
pub mod error;
pub mod handle;
pub mod runtime;
pub mod sink;

pub use error::RuntimeError;
pub use handle::StoreHandle;
pub use runtime::Runtime;
pub use sink::{NullSink, RecordingSink, Sink, SinkError};

/// These bounds are load-bearing: the core thread owns the [`duet_core::Store`]
/// and receives values from other threads. Asserted here so a change that
/// breaks them fails at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuntimeError>();
    assert_send_sync::<StoreHandle>();
    assert_send_sync::<Runtime>();
};
