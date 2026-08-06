//! The message envelope between Duet's host and its guests.
//!
//! A Flutter renderer and a JavaScript webview both talk to one Rust host.
//! This crate defines what they say, and [`dispatch()`] serves it.
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
//! # Reading state, and running logic
//!
//! Four of the five request kinds address the store. The fifth,
//! [`Request::Invoke`], runs a **host command** — logic the guest cannot
//! perform itself — and has three possible answers rather than two:
//!
//! | Answer | Means |
//! |---|---|
//! | [`Response::Returned`] | the command ran and returned `Ok` |
//! | [`Response::Raised`] | the command ran and returned `Err`, carried as a typed value |
//! | [`Response::Failed`] | the host refused: no such command, bad arguments, a body that panicked |
//!
//! `raised` and `failed` are deliberately different kinds. Collapsing a
//! developer's typed error into a `failed`'s prose is not reversible, and the
//! two say different things about whether retrying is safe.
//!
//! [`dispatch()`] and [`handle_text()`] register no commands and answer every
//! `invoke` with a `failed`; [`dispatch_with()`] and [`handle_text_with()`] take
//! a [`CommandHost`]. Which commands a guest may reach is decided entirely by
//! which host its *surface* was built with — see [`CommandHost`].
//!
//! # Untrusted input
//!
//! Guests are separate processes and their messages are untrusted. Every decode
//! path is total: malformed bytes produce an error, never a panic. And
//! [`Request::Subscribe`] deliberately carries no `SubscriberId` — the host
//! supplies it, so one guest cannot subscribe as another. [`Request::Invoke`]
//! carries no caller identity for the same reason.
//!
//! A command body is the one thing here that is *not* this crate's code, so the
//! two ways it can break the host — panicking, and returning a value too deep
//! for any envelope — are caught at a single choke point rather than asked of
//! each implementor. See [`command`].

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod command;
pub mod dispatch;
pub mod message;
pub mod text;
mod wire;

pub use command::{CommandHost, NoCommands, Outcome};

pub use dispatch::{dispatch, dispatch_with};

pub use message::{Args, Push, Request, RequestId, Response};

pub use text::{handle_text, handle_text_with, push_text};

/// Encodes a [`Request`] for transmission.
pub fn encode_request(request: &Request) -> serde_json::Value {
    wire::encode_request(request)
}

/// Decodes a [`Request`] received from a guest.
///
/// # Errors
///
/// A [`duet_codec::CodecError`] describing the first structural problem found.
/// Total over all JSON input: never panics, whatever a guest sends.
pub fn decode_request(json: &serde_json::Value) -> Result<Request, duet_codec::CodecError> {
    wire::decode_request(json)
}

/// Encodes a [`Response`] for transmission.
pub fn encode_response(response: &Response) -> serde_json::Value {
    wire::encode_response(response)
}

/// Decodes a [`Response`] received from the host.
///
/// # Errors
///
/// A [`duet_codec::CodecError`] describing the first structural problem found.
/// Total over all JSON input: never panics, whatever the host sends.
pub fn decode_response(json: &serde_json::Value) -> Result<Response, duet_codec::CodecError> {
    wire::decode_response(json)
}

/// Encodes a [`Push`] for transmission.
pub fn encode_push(push: &Push) -> serde_json::Value {
    wire::encode_push(push)
}

/// Decodes a [`Push`] received from the host.
///
/// # Errors
///
/// A [`duet_codec::CodecError`] describing the first structural problem found.
/// Total over all JSON input: never panics, whatever the host sends.
pub fn decode_push(json: &serde_json::Value) -> Result<Push, duet_codec::CodecError> {
    wire::decode_push(json)
}
