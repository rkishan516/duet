//! Transport-agnostic webview guest logic for Duet.
//!
//! A webview guest (a `wry` `WebView` on macOS, and eventually Windows and
//! Linux equivalents) exchanges JSON text with the host: it posts IPC
//! messages and the host evaluates JavaScript back into it. Request routing
//! and decode-and-recover error handling live in `duet_protocol::text`
//! instead, because they have nothing webview-specific about them — a
//! Flutter guest needs the exact same handling on its platform channel. What
//! stays here is the JavaScript-specific plumbing: the strings that wrap a
//! response or a push so a `wry` `WebView` can receive them, and the guest's
//! HTML/JS bootstrap.
//!
//! Every item here is `pub`: this crate has no internal module structure to
//! hide behind beyond the [`bootstrap`] module, so its public API *is* its
//! implementation.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

use duet_protocol::Push;

pub mod bootstrap;

/// Wraps a response in JavaScript that hands it to the guest.
pub fn response_script(reply_json: &str) -> String {
    format!("window.__duet && window.__duet.onResponse({reply_json});")
}

/// Wraps a push in JavaScript that hands it to the guest.
pub fn push_script(push: &Push) -> String {
    format!(
        "window.__duet && window.__duet.onPush({});",
        duet_protocol::push_text(push)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriberId, Value};

    #[test]
    fn a_push_is_framed_as_js_that_calls_the_guest_hook() {
        let note = duet_core::Notification {
            subscriber: SubscriberId(1),
            subscription: duet_core::SubscriptionId(2),
            patch: duet_core::Patch {
                path: Path::parse("editor.zoom").expect("path"),
                value: Value::Float(2.0),
            },
        };
        let js = push_script(&duet_protocol::Push::Notification(note));
        assert!(
            js.contains("__duet.onPush"),
            "a push must call the guest's hook, got {js}"
        );
        assert!(
            js.contains("\"t\":\"f\""),
            "the payload must be the tagged encoding, got {js}"
        );
    }
}
