//! The webview surface: a `wry` WebView speaking `duet-protocol`.
//!
//! This module currently holds only the request-routing logic, exercised
//! directly by the tests below. Nothing in the crate calls it yet — wiring it
//! to a real `wry::WebView` (IPC handler registration, script evaluation) is
//! a later task, so every item here is `pub(crate)` and otherwise unused
//! outside tests until that lands.
#![allow(dead_code)]

use duet_core::SubscriberId;
use duet_protocol::{Push, RequestId, Response};
use duet_runtime::StoreHandle;

/// Serves one IPC message and returns the JSON text to send back.
///
/// Total by construction: malformed input becomes a [`Response::Failed`], so a
/// guest always receives well-formed JSON. `duet_protocol::dispatch` is itself
/// infallible, so the only failures possible here are decoding ones.
///
/// `subscriber` is the surface's own, supplied by the host. A `subscriber`
/// field appearing in the message is **ignored** — `duet_protocol::Request`
/// has no such field, so a guest cannot subscribe as another guest.
pub(crate) fn handle_ipc_text(store: &StoreHandle, subscriber: SubscriberId, text: &str) -> String {
    let response = match decode(text) {
        Ok(request) => duet_protocol::dispatch(store, subscriber, request),
        Err((id, message)) => Response::Failed { id, message },
    };
    // `encode_response` produces a plain JSON object; serializing it here is
    // the single encoding step. Note `wry`'s `evaluate_script_with_callback`
    // re-serializes anything a script *returns*, which is why responses are
    // pushed rather than returned — see `response_script`.
    serde_json::to_string(&duet_protocol::encode_response(&response))
        .unwrap_or_else(|_| FALLBACK_FAILURE.to_string())
}

/// Emitted only if serializing a `Response` itself fails, which cannot happen
/// for the shapes `encode_response` produces. Present so this function can be
/// total without an `expect`.
const FALLBACK_FAILURE: &str =
    r#"{"kind":"failed","id":"0","message":"host could not serialize its response"}"#;

/// Decodes a request, recovering the correlation id where possible.
///
/// A guest waits on its request id. When the body is undecodable but the id is
/// readable, returning it lets the guest fail that specific call instead of
/// hanging.
fn decode(text: &str) -> Result<duet_protocol::Request, (RequestId, String)> {
    let json: serde_json::Value = match serde_json::from_str(text) {
        Ok(j) => j,
        Err(e) => return Err((RequestId(0), format!("malformed JSON: {e}"))),
    };
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .map(RequestId)
        .unwrap_or(RequestId(0));

    duet_protocol::decode_request(&json).map_err(|e| (id, e.to_string()))
}

/// Wraps a response in JavaScript that hands it to the guest.
pub(crate) fn response_script(reply_json: &str) -> String {
    format!("window.__duet && window.__duet.onResponse({reply_json});")
}

/// Wraps a push in JavaScript that hands it to the guest.
pub(crate) fn push_script(push: &Push) -> String {
    let encoded = serde_json::to_string(&duet_protocol::encode_push(push))
        .unwrap_or_else(|_| "null".to_string());
    format!("window.__duet && window.__duet.onPush({encoded});")
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::{Path, SubscriberId, Value};
    use duet_runtime::{NullSink, Runtime};

    fn rt() -> Runtime {
        Runtime::spawn(
            Value::map([("editor", Value::map([("zoom", Value::Float(1.0))]))]),
            NullSink,
        )
    }

    #[test]
    fn a_get_request_is_answered_with_the_stored_value() {
        let rt = rt();
        let reply = handle_ipc_text(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"get","id":"1","path":"editor.zoom"}"#,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("the reply must be valid JSON");
        assert_eq!(parsed["kind"], "value");
        assert_eq!(parsed["id"], "1");
        assert_eq!(parsed["value"], serde_json::json!({"t": "f", "v": 1.0}));
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_set_request_writes_and_is_visible_to_rust() {
        // The shared-state claim, at the smallest scale that can express it.
        let rt = rt();
        let handle = rt.handle();
        let reply = handle_ipc_text(
            &handle,
            SubscriberId(1),
            r#"{"kind":"set","id":"2","path":"editor.zoom","value":{"t":"f","v":4.5}}"#,
        );
        assert!(
            reply.contains("\"done\""),
            "expected a done response, got {reply}"
        );
        assert_eq!(
            handle
                .get(&Path::parse("editor.zoom").expect("path"))
                .expect("read should succeed"),
            Some(Value::Float(4.5)),
            "a value written over IPC must be readable from Rust"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn malformed_ipc_text_produces_a_failed_response_not_a_panic() {
        // This is untrusted guest input arriving on a channel we do not control.
        let rt = rt();
        for bad in [
            "",
            "not json",
            "42",
            "{}",
            r#"{"kind":"nope","id":"1"}"#,
            r#"{"kind":"get","id":"1","path":"a.[0]"}"#,
            r#"{"kind":"get","id":1,"path":"a"}"#,
        ] {
            let reply = handle_ipc_text(&rt.handle(), SubscriberId(1), bad);
            let parsed: serde_json::Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("reply for {bad:?} must be valid JSON: {e}"));
            assert_eq!(
                parsed["kind"], "failed",
                "input {bad:?} should produce a failed response, got {reply}"
            );
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_unparseable_request_still_echoes_an_id_when_one_is_present() {
        // A guest correlates by id. If we cannot decode the body but the id is
        // readable, echo it so the guest can fail that specific call rather
        // than waiting forever.
        let rt = rt();
        let reply = handle_ipc_text(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"get","id":"77","path":"a.[0]"}"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["kind"], "failed");
        assert_eq!(
            parsed["id"], "77",
            "the failure must name the request it answers"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_handler_ignores_any_subscriber_named_on_the_wire() {
        // The security property: a guest cannot subscribe as another guest.
        // Even with a `subscriber` field present, the handler must use the one
        // it was constructed with.
        let rt = rt();
        let handle = rt.handle();
        let reply = handle_ipc_text(
            &handle,
            SubscriberId(7),
            r#"{"kind":"subscribe","id":"3","path":"editor.zoom","subscriber":"999"}"#,
        );
        assert!(reply.contains("\"subscribed\""), "got {reply}");

        // The subscription must belong to 7, not 999.
        assert_eq!(
            handle.drop_subscriber(SubscriberId(999)).expect("query"),
            0,
            "no subscription may be attributed to a guest-named subscriber"
        );
        assert_eq!(
            handle.drop_subscriber(SubscriberId(7)).expect("query"),
            1,
            "the subscription must belong to the host-supplied subscriber"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

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

    /// A guest that speaks the webview IPC channel may be running untrusted
    /// web content — `request.body()` on the `wry` side is attacker-controlled
    /// text, not a value this host constructed. `handle_ipc_text` sits
    /// directly on that trust boundary, so it must be total against
    /// *hostile* input, not merely against a handful of hand-picked bad
    /// strings: no panic, no hang (deep recursion must be rejected, not
    /// walked), and no unbounded echo (a guest must not be able to turn a
    /// megabyte of garbage into a megabyte of host-produced text, e.g. in a
    /// log line downstream of this function).
    #[test]
    fn hostile_guest_input_cannot_panic_hang_or_echo_unbounded_text() {
        // The reply to a rejected request is a short, fixed-shape JSON
        // object; it must never scale with the size of the offending input.
        // 4096 is generous headroom over the ~100-byte replies this actually
        // produces — the point is "bounded", not a tight bound.
        const MAX_REASONABLE_REPLY: usize = 4096;

        let rt = rt();
        let handle = rt.handle();

        let assert_failed = |text: &str, what: &str| -> serde_json::Value {
            let reply = handle_ipc_text(&handle, SubscriberId(1), text);
            let parsed: serde_json::Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("{what}: reply must be valid JSON, got {e}: {reply}"));
            assert_eq!(
                parsed["kind"], "failed",
                "{what}: expected a failed response, got {reply}"
            );
            parsed
        };

        // 200,000 unclosed `[` would overflow the stack of a naive recursive
        // descent parser well before reaching the end of input. serde_json's
        // own recursion-limit guard must reject this instead.
        assert_failed(&"[".repeat(200_000), "200,000-deep nested array");

        // The same guard must hold for a tagged `Value` nested 5,000 deep
        // inside a real `set` request, not just for raw JSON structure.
        let mut nested_value = "null".to_string();
        for _ in 0..5_000 {
            nested_value = format!(r#"{{"t":"m","v":{{"a":{nested_value}}}}}"#);
        }
        assert_failed(
            &format!(r#"{{"kind":"set","id":"1","path":"a","value":{nested_value}}}"#),
            "5,000-deep nested tagged value",
        );

        // A 1 MB path must not be echoed back whole: the failure it produces
        // has to stay small regardless of how large the offending path was.
        let huge_path = format!(
            r#"{{"kind":"get","id":"1","path":"{}]"}}"#,
            "a".repeat(1_000_000)
        );
        let reply = handle_ipc_text(&handle, SubscriberId(1), &huge_path);
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("1 MB path: reply must be valid JSON");
        assert_eq!(parsed["kind"], "failed", "1 MB path: got {reply}");
        assert!(
            reply.len() < MAX_REASONABLE_REPLY,
            "1 MB path: reply must not echo the guest's text, got {} bytes",
            reply.len()
        );

        // Likewise for a 1 MB bogus value tag inside a `set`.
        let huge_tag = format!(
            r#"{{"kind":"set","id":"1","path":"a","value":{{"t":"{}","v":null}}}}"#,
            "z".repeat(1_000_000)
        );
        let reply = handle_ipc_text(&handle, SubscriberId(1), &huge_tag);
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("1 MB value tag: reply must be valid JSON");
        assert_eq!(parsed["kind"], "failed", "1 MB value tag: got {reply}");
        assert!(
            reply.len() < MAX_REASONABLE_REPLY,
            "1 MB value tag: reply must not echo the guest's text, got {} bytes",
            reply.len()
        );

        // A lone UTF-16 surrogate and a raw control character are both
        // things a hostile guest can put in a JSON string; neither may panic
        // the JSON parser.
        assert_failed(
            r#"{"kind":"get","id":"1","path":"\ud800"}"#,
            "lone UTF-16 surrogate",
        );
        assert_failed(
            "{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\u{0007}\"}",
            "raw control character",
        );

        rt.shutdown().expect("shutdown should succeed");
    }
}
