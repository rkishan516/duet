//! Platform-free core of the Duet framework.
//!
//! Contains the observable state store, the surface lifecycle state machine, and
//! policy evaluation. This crate has no platform dependencies and no runtime
//! dependencies, so all behaviour here is testable with plain `cargo test`.

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
