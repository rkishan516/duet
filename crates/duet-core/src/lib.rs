//! Platform-free core of the Duet framework.
//!
//! Contains the observable state store, the surface lifecycle state machine, and
//! policy evaluation. This crate has no platform dependencies and no runtime
//! dependencies, so all behaviour here is testable with plain `cargo test`.

pub mod path;

pub use path::{Path, PathParseError, Segment};
