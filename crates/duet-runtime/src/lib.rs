//! Threading runtime for Duet.
//!
//! [`duet_core::Store`] is deliberately single-threaded plain data. This crate
//! moves it onto a dedicated **core thread** and hands out cheap, cloneable
//! `StoreHandle`s that any thread may use.
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
//! Notifications are handed to a `Sink`. That is a trait rather than `tao`'s
//! `EventLoopProxy` so this crate stays testable without a window system;
//! Phase 2b supplies the real implementation.

#![deny(missing_docs)]

mod command;
pub mod error;
pub mod sink;

pub use error::RuntimeError;
pub use sink::{NullSink, RecordingSink, Sink, SinkError};

/// These bounds are load-bearing: the core thread owns the `Store` and receives
/// values from other threads. Asserted here so a change that breaks them fails
/// at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RuntimeError>();
};
