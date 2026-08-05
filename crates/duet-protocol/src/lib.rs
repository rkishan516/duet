//! The message envelope between Duet's host and its guests.
//!
//! A Flutter renderer and a JavaScript webview both talk to one Rust host.
//! This crate defines what they say, and `dispatch` serves it.
//!
//! # Three message kinds
//!
//! | Kind | Direction | Correlated |
//! |---|---|---|
//! | [`Request`] | guest → host | by [`RequestId`] |
//! | [`Response`] | host → guest | echoes the id |
//! | [`Push`] | host → guest | no |
//!
//! [`Push`] is separate because it answers nothing — it arrives because
//! something the guest subscribed to changed.
//!
//! # Untrusted input
//!
//! Guests are separate processes and their messages are untrusted. Every decode
//! path is total: malformed bytes produce an error, never a panic. And
//! [`Request::Subscribe`] deliberately carries no `SubscriberId` — the host
//! supplies it, so one guest cannot subscribe as another.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod message;

pub use message::{Push, Request, RequestId, Response};
