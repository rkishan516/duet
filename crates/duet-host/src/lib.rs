//! Wires Duet's store, supervisor and platform together.
//!
//! [`duet_supervisor::Supervisor`] decides *when* a renderer should start,
//! suspend or be torn down; [`duet_runtime`] holds the state. This crate is
//! what connects them to a platform: it translates platform events into
//! supervisor input, executes the supervisor's decisions against a
//! `WindowBackend`, and discharges the one obligation the supervisor cannot.
//!
//! # The teardown obligation
//!
//! `duet_supervisor::SurfaceAction::Teardown` documents that the host must also
//! drop the surface's store subscriptions — the supervisor holds no store
//! handle. Nothing links a `SurfaceId` to a `SubscriberId`, so that mapping
//! lives here. Forgetting it leaves the store computing and delivering
//! notifications for a renderer that no longer exists.
//!
//! # Why the platform is behind a trait
//!
//! Creating a window needs a display; deciding *which* window to create does
//! not. [`WindowBackend`] is that line, and it is the same seam that let
//! `duet-runtime` and `duet-supervisor` be tested on a machine with no
//! reachable window server. The real `tao`/`wry` backend arrives in Phase 2b-3.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod backend;

pub use backend::{BackendCall, BackendError, RecordingBackend, WindowBackend};

/// These bounds are load-bearing: a backend is moved onto the main thread and
/// its recording shared with assertions. Asserted here so a change that breaks
/// them fails at its own source rather than at an integration point.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BackendError>();
    assert_send_sync::<BackendCall>();
    assert_send_sync::<RecordingBackend>();
};
