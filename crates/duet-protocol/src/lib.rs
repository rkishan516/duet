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
mod wire;

pub use message::{Push, Request, RequestId, Response};

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
