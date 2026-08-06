//! The typed layer over a running store.
//!
//! Every type here is hand-written and hand-tested. Nothing in it is generated,
//! and nothing generated will contain logic — increments 4 and 6 emit
//! declarations and string literals that call into this.

pub mod error;
pub mod field;
pub mod reading;
pub mod store;
