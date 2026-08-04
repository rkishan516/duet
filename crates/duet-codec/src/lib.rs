//! Wire format for Duet.
//!
//! Encodes the types that cross the IPC boundary between the Rust host and its
//! Flutter and JavaScript guests. **This crate decodes untrusted input** — every
//! decode path is total: malformed bytes produce a [`CodecError`], never a panic.
//!
//! # Why the encoding is tagged
//!
//! Every value encodes as `{"t":"<tag>","v":<payload>}`. Plain JSON cannot
//! represent [`duet_core::Value`] faithfully: `Bytes` and `Str` would collapse
//! into one JSON string, `Int(1)` and `Float(1.0)` would both become `1`, and
//! `NaN` has no JSON form at all — it would decode back as `Null`, changing the
//! *variant* rather than the magnitude.
//!
//! `Int` is carried as a **string**, not a JSON number, because JavaScript
//! numbers are IEEE-754 doubles: an `i64` above 2^53 would lose precision in the
//! webview while surviving intact in Dart. Two guests disagreeing about a value
//! is the worst kind of bug this format could ship.
//!
//! Verbosity is an accepted cost. Payloads are small patches, guests never see
//! the wire format directly (Phase 4 generates typed accessors over it), and the
//! `Codec` trait exists so a compact binary encoding can replace this one
//! without touching a public API.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod base64;
pub mod error;

pub use error::CodecError;
