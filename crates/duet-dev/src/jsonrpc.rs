//! JSON-RPC 2.0 request building and reply correlation, as pure functions.
//!
//! The Dart VM service multiplexes two things onto one socket: replies to
//! requests, and a stream of unsolicited events (isolate pause/resume, GC,
//! logging, extension events). A client that assumed the next frame after its
//! request was its reply would work right up until the first GC.
//!
//! So correlation is explicit, and it is a pure function over one message and
//! the id being waited for — which means every case a real socket would
//! produce only rarely (an error object, a reply to an id we already
//! abandoned, a notification with no `id` at all, a malformed frame) is a
//! two-line test here instead of a race nobody can reproduce.

use serde_json::{Value, json};

/// Encodes one JSON-RPC 2.0 request.
pub(crate) fn request(id: u64, method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

/// What one inbound message means to a caller waiting on `id`.
#[derive(Debug, PartialEq)]
pub(crate) enum Correlation {
    /// Our reply, with its `result`.
    Reply(Box<Value>),
    /// Our reply, but the server reported an error. The string is rendered
    /// from the JSON-RPC `error` object.
    Failed(String),
    /// Somebody else's message: an event, or a reply to a different id. Keep
    /// waiting.
    Other,
    /// The frame was not usable JSON-RPC at all.
    Undecodable(String),
}

/// Decides what `text` means to a caller waiting on `id`.
///
/// A reply whose `id` matches but which carries neither `result` nor `error`
/// is [`Correlation::Failed`], not [`Correlation::Other`]: it is definitely
/// ours, and treating it as somebody else's would hang the caller until its
/// deadline rather than reporting the malformed reply immediately.
pub(crate) fn correlate(text: &str, id: u64) -> Correlation {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Correlation::Undecodable(text.chars().take(200).collect());
    };

    // The VM service sends ids as JSON numbers; the spec also permits strings,
    // and a stricter reading of our own id back as a string should still
    // correlate. Both are accepted, nothing else is.
    let matches = match value.get("id") {
        Some(Value::Number(n)) => n.as_u64() == Some(id),
        Some(Value::String(s)) => s.parse::<u64>() == Ok(id),
        _ => false,
    };
    if !matches {
        return Correlation::Other;
    }

    if let Some(error) = value.get("error") {
        return Correlation::Failed(render_error(error));
    }
    match value.get("result") {
        Some(result) => Correlation::Reply(Box::new(result.clone())),
        None => Correlation::Failed(format!(
            "reply to id {id} carried neither `result` nor `error`: {}",
            truncated(&value)
        )),
    }
}

/// Renders a JSON-RPC `error` object as one line.
///
/// The VM service puts the interesting part in `data.details` (a Dart stack
/// trace, or the reason a reload was refused), so that is preferred over the
/// generic `message` when present.
fn render_error(error: &Value) -> String {
    let code = error.get("code").and_then(Value::as_i64);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message");
    let details = error
        .get("data")
        .and_then(|d| d.get("details"))
        .and_then(Value::as_str);
    match (code, details) {
        (Some(code), Some(details)) => format!("error {code}: {message} — {details}"),
        (Some(code), None) => format!("error {code}: {message}"),
        (None, Some(details)) => format!("error: {message} — {details}"),
        (None, None) => format!("error: {message}"),
    }
}

/// A short rendering of a value, for error messages.
fn truncated(value: &Value) -> String {
    value.to_string().chars().take(200).collect()
}

#[cfg(test)]
#[path = "jsonrpc_tests.rs"]
mod tests;
