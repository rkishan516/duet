//! The [`SharedState`](crate::SharedState) impls Duet ships.
//!
//! Split by shape rather than by trait method so that the reason a family is
//! accepted — and, by its absence, the reason a neighbouring family is not —
//! sits next to the code that accepts it.

mod collection;
mod scalar;
mod wrapper;
