//! Decides when Duet surfaces start, suspend and are torn down.
//!
//! [`duet_core`] provides a lifecycle state machine and a teardown policy
//! evaluator, both pure functions. This crate is what drives them against real
//! surfaces: it tracks each surface's state and window counts, consumes host
//! events, and on each `Supervisor::tick` returns the [`SurfaceAction`]s the
//! host must perform.
//!
//! # Decisions, not effects
//!
//! The supervisor never acts. Starting a renderer needs a window server;
//! deciding that one *should* start does not. Returning actions as data keeps
//! this crate testable on any machine, lets the host choose which thread
//! performs the work, and makes every decision directly assertable in a test.
//!
//! # What teardown is for
//!
//! Spike A measured the Flutter side: a booted engine with an attached view
//! holds 223 MB, detaching the view still holds 223 MB, and only shutting the
//! engine down drops it to 104 MB. So [`SurfaceAction::Teardown`] is what
//! delivers the framework's headline claim, and the `Suspending` grace period
//! exists purely to avoid paying a ~180 ms engine boot when a user closes and
//! immediately reopens a window.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod action;
pub mod id;

pub use action::SurfaceAction;
pub use id::{SurfaceId, SurfaceIdAllocator};

/// These bounds are load-bearing: a host will tick the supervisor from its
/// event loop while holding it alongside other state. Asserted here so a change
/// that breaks them fails at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SurfaceId>();
    assert_send_sync::<SurfaceAction>();
    assert_send_sync::<SurfaceIdAllocator>();
};
