//! Text-level request routing: decode, dispatch, encode.
//!
//! A guest that exchanges JSON *text* with the host — rather than calling
//! [`crate::dispatch()`] directly against in-process values — needs decode-and-
//! recover error handling and a uniform reply encoding on top of it. That is
//! transport-agnostic: nothing here knows whether the text arrived over a
//! webview's IPC channel or a Flutter platform channel, which is why it lives
//! in this crate rather than in a transport-specific one.

use duet_core::SubscriberId;
use duet_runtime::StoreHandle;

use crate::message::{Push, RequestId, Response};

/// Serves one guest message and returns the JSON text to send back.
///
/// Total by construction: malformed input becomes a [`Response::Failed`], so a
/// guest always receives well-formed JSON. [`crate::dispatch()`] is itself
/// infallible, so the only failures possible here are decoding ones.
///
/// `subscriber` is the surface's own, supplied by the host. A `subscriber`
/// field appearing in the message is **ignored** — [`crate::Request`] has no
/// such field, so a guest cannot subscribe as another guest.
pub fn handle_text(store: &StoreHandle, subscriber: SubscriberId, text: &str) -> String {
    let response = match decode(text) {
        Ok(request) => crate::dispatch(store, subscriber, request),
        Err((id, message)) => Response::Failed { id, message },
    };
    // `encode_response` produces a plain JSON object; serializing it here is
    // the single encoding step. Note `wry`'s `evaluate_script_with_callback`
    // re-serializes anything a script *returns*, which is why responses are
    // pushed rather than returned — see `duet_webview::response_script`.
    serde_json::to_string(&crate::encode_response(&response))
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
///
/// "Readable" means it passes [`duet_codec::parse_wire_id`] — canonically
/// spelled *and* within the wire's id domain. Anything else is not recovered,
/// and the failure carries `RequestId(0)` instead.
///
/// Recovering `7` from `"007"` would reintroduce the very mismatch the
/// canonical rule exists to prevent: the reply would name an id the guest never
/// sent, so a guest keying its pending map by the string it sent would hang
/// anyway — while this function's whole purpose is to stop exactly that hang.
/// An id above `duet_codec::MAX_WIRE_ID` is refused for the mirrored reason:
/// echoing it back would send a Dart guest a number its own `int` cannot parse,
/// so the "recovery" would fail on arrival. `RequestId(0)` is the honest answer
/// in both cases: this reply answers no request the guest can name.
///
/// # The depth pre-scan runs before the parser, not after it
///
/// [`duet_codec::exceeds_max_json_depth`] reads the raw text first. Two reasons
/// it is not left to `serde_json`, whose own recursion limit happens to sit at
/// the same 127 today:
///
/// - **The host must own its own limit.** Three implementations of this format
///   have to agree on the boundary exactly, and `corpus/wire-corpus.json` pins
///   it. A `serde_json` upgrade that moved its internal constant would move the
///   host's behaviour with it and silently re-open a cross-language divergence
///   that nothing in this workspace would fail on.
/// - **Rejecting before parsing is cheaper and stricter.** Over-deep text never
///   becomes a tree at all.
///
/// Over-deep text is reported exactly like any other unparseable input: the id
/// is *not* recovered, because recovering it would mean parsing the very
/// document being refused. `RequestId(0)` again means "this reply answers no
/// request you can name" — which a guest must handle, see
/// `crates/duet-webview/src/bootstrap.rs`.
fn decode(text: &str) -> Result<crate::Request, (RequestId, String)> {
    if duet_codec::exceeds_max_json_depth(text) {
        return Err((
            RequestId(0),
            format!(
                "malformed JSON: nests deeper than {} containers",
                duet_codec::MAX_JSON_DEPTH
            ),
        ));
    }
    let json: serde_json::Value = match serde_json::from_str(text) {
        Ok(j) => j,
        Err(e) => return Err((RequestId(0), format!("malformed JSON: {e}"))),
    };
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(duet_codec::parse_wire_id)
        .map(RequestId)
        .unwrap_or(RequestId(0));

    crate::decode_request(&json).map_err(|e| (id, e.to_string()))
}

/// Encodes a push as the JSON text a guest receives.
///
/// A webview guest needs this wrapped in JavaScript (see
/// `duet_webview::push_script`); a Flutter guest receives it verbatim on its
/// platform channel, which is why the encoding lives here rather than in a
/// transport crate.
pub fn push_text(push: &Push) -> String {
    serde_json::to_string(&crate::encode_push(push)).unwrap_or_else(|_| "null".to_string())
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
        let reply = handle_text(
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
        let reply = handle_text(
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
            let reply = handle_text(&rt.handle(), SubscriberId(1), bad);
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
        let reply = handle_text(
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
    fn a_non_canonical_id_is_not_recovered() {
        // Recovery exists so a guest can fail one specific call. Recovering
        // `7` from `"007"` would defeat that: the guest keys its pending map
        // by the string it SENT, so a reply carrying `"7"` never matches and
        // the call hangs anyway — silently. `RequestId(0)` is the honest
        // answer: "this reply answers no request you can name".
        let rt = rt();
        for raw in ["007", "+1", "0000000000000000000007", " 1", "1 ", ""] {
            let text = format!(r#"{{"kind":"get","id":"{raw}","path":"a.[0]"}}"#);
            let reply = handle_text(&rt.handle(), SubscriberId(1), &text);
            let parsed: serde_json::Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("reply for {raw:?} must be valid JSON: {e}"));
            assert_eq!(parsed["kind"], "failed", "id {raw:?} must fail: {reply}");
            assert_eq!(
                parsed["id"], "0",
                "a non-canonical id {raw:?} must NOT be recovered, got {reply}"
            );
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_handler_ignores_any_subscriber_named_on_the_wire() {
        // The security property: a guest cannot subscribe as another guest.
        // Even with a `subscriber` field present, the handler must use the
        // one it was constructed with. One call alone cannot distinguish
        // "uses the parameter" from "hardcodes the subscriber it happens to
        // see first", so this drives the handler with two different
        // host-supplied subscribers, both carrying the same bogus
        // `"subscriber":"999"` on the wire, and checks the subscription
        // count landed on each one respectively.
        let rt = rt();
        let handle = rt.handle();

        let reply_a = handle_text(
            &handle,
            SubscriberId(7),
            r#"{"kind":"subscribe","id":"3","path":"editor.zoom","subscriber":"999"}"#,
        );
        assert!(reply_a.contains("\"subscribed\""), "got {reply_a}");

        let reply_b = handle_text(
            &handle,
            SubscriberId(42),
            r#"{"kind":"subscribe","id":"4","path":"editor.zoom","subscriber":"999"}"#,
        );
        assert!(reply_b.contains("\"subscribed\""), "got {reply_b}");

        // Neither subscription may be attributed to the guest-named subscriber.
        assert_eq!(
            handle.drop_subscriber(SubscriberId(999)).expect("query"),
            0,
            "no subscription may be attributed to a guest-named subscriber"
        );
        // Each subscription must belong to the host-supplied subscriber that
        // made its call, not to whichever one the handler happens to favor.
        assert_eq!(
            handle.drop_subscriber(SubscriberId(7)).expect("query"),
            1,
            "the first subscription must belong to the host-supplied subscriber 7"
        );
        assert_eq!(
            handle.drop_subscriber(SubscriberId(42)).expect("query"),
            1,
            "the second subscription must belong to the host-supplied subscriber 42"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn the_nesting_limit_bites_at_exactly_one_container_past_the_bound() {
        // THE regression test for a one-level divergence. Both guests enforced
        // 128 where this host stops at 127, so a document with exactly 128
        // containers was accepted by Dart and TypeScript and refused here — and
        // no test could see it, because the only nesting cases in the suite
        // used 200 and 5,000 levels, which every implementation refuses
        // whatever its off-by-one. Pin the exact boundary or pin nothing.
        //
        // The discriminator is the echoed id, not the response kind: a request
        // that got *parsed* has its id recovered, while one refused by the
        // pre-scan answers `RequestId(0)` because reading its id would mean
        // parsing the document being refused. That stays true regardless of
        // what the store later decides about a value this deep.
        /// A `set` request whose document nests exactly `containers` JSON
        /// containers: one for the envelope, then a tagged value carrying the
        /// rest. A tagged value costs two containers per level of its own
        /// nesting, so an odd remainder ends in `{"t":"n"}` and an even one in
        /// an empty list. Both fixtures come from this one generator, so they
        /// differ by exactly one container and by nothing else.
        fn set_request(containers: usize) -> String {
            let value = containers - 1;
            let wrappers = (value - 1) / 2;
            let leaf = if value % 2 == 0 {
                r#"{"t":"l","v":[]}"#
            } else {
                r#"{"t":"n"}"#
            };
            format!(
                r#"{{"kind":"set","id":"1","path":"editor","value":{}{}{}}}"#,
                r#"{"t":"l","v":["#.repeat(wrappers),
                leaf,
                r#"]}"#.repeat(wrappers),
            )
        }

        let rt = rt();
        let handle = rt.handle();

        let at_limit = set_request(duet_codec::MAX_JSON_DEPTH);
        assert!(
            !duet_codec::exceeds_max_json_depth(&at_limit),
            "the fixture must sit AT the limit, not past it"
        );
        let reply = handle_text(&handle, SubscriberId(1), &at_limit);
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(
            parsed["id"],
            "1",
            "a document at exactly {} containers must be parsed, got {reply}",
            duet_codec::MAX_JSON_DEPTH
        );

        let over_limit = set_request(duet_codec::MAX_JSON_DEPTH + 1);
        assert!(
            duet_codec::exceeds_max_json_depth(&over_limit),
            "the fixture must sit exactly one container past the limit"
        );
        let reply = handle_text(&handle, SubscriberId(1), &over_limit);
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["kind"], "failed", "got {reply}");
        assert_eq!(
            parsed["id"], "0",
            "over-deep text must not have its id recovered, got {reply}"
        );
        assert!(
            reply.contains(&duet_codec::MAX_JSON_DEPTH.to_string()),
            "the failure must name the limit it enforced, got {reply}"
        );

        rt.shutdown().expect("shutdown should succeed");
    }

    /// A guest that speaks the webview IPC channel may be running untrusted
    /// web content — `request.body()` on the `wry` side is attacker-controlled
    /// text, not a value this host constructed. `handle_text` sits
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
            let reply = handle_text(&handle, SubscriberId(1), text);
            let parsed: serde_json::Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("{what}: reply must be valid JSON, got {e}: {reply}"));
            assert_eq!(
                parsed["kind"], "failed",
                "{what}: expected a failed response, got {reply}"
            );
            parsed
        };

        // 200,000 unclosed `[` would overflow the stack of a naive recursive
        // descent parser — or of a naive recursive depth *check* — well before
        // reaching the end of input. `decode`'s iterative pre-scan must reject
        // it instead.
        assert_failed(&"[".repeat(200_000), "200,000-deep nested array");

        // WARNING to future maintainers: this case looks like it stresses
        // `duet_codec`'s recursive tagged-value decoder specifically, by
        // nesting a `Value` 5,000 deep inside a real `set` request rather
        // than nesting raw JSON structure. It does not. `decode` (above)
        // runs `duet_codec::exceeds_max_json_depth` over the raw text before
        // any of it reaches `duet_protocol::decode_request` — and therefore
        // `duet_codec`'s decoder — so this hits exactly the same guard as the
        // 200,000-bracket case above, just via a longer string.
        // `duet_codec::decode_value`'s recursion is structurally unreachable
        // from guest *text* at more than `MAX_JSON_DEPTH / 2` levels, because
        // the pre-scan in front of it is the depth guard. If that pre-scan
        // were ever removed, that recursion becomes reachable — and this case
        // would then need a real assertion on the codec's own behavior, not
        // just on this text-level guard.
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
        let reply = handle_text(&handle, SubscriberId(1), &huge_path);
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("1 MB path: reply must be valid JSON");
        assert_eq!(parsed["kind"], "failed", "1 MB path: got {reply}");
        assert!(
            reply.len() < MAX_REASONABLE_REPLY,
            "1 MB path: reply must not echo the guest's text, got {} bytes",
            reply.len()
        );

        // A path that *parses* but cannot *resolve* is the case the huge-path
        // check above misses entirely: that one dies in `Path::parse`, which
        // reports only a byte offset and one character, so it never reaches
        // `duet_core::SetError` — whose `Display` renders the path itself.
        // A `set` at `a.<1 MB>` parses fine and fails resolving `a`, taking the
        // `SetError::MissingKey` route straight into the reply.
        let unresolvable_path = format!(
            r#"{{"kind":"set","id":"1","path":"a.{}","value":{{"t":"i","v":"1"}}}}"#,
            "k".repeat(1_000_000)
        );
        let reply = handle_text(&handle, SubscriberId(1), &unresolvable_path);
        let parsed: serde_json::Value = serde_json::from_str(&reply)
            .expect("1 MB parseable-but-unresolvable path: reply must be valid JSON");
        assert_eq!(
            parsed["kind"], "failed",
            "1 MB parseable-but-unresolvable path: got {reply}"
        );
        assert!(
            reply.len() < MAX_REASONABLE_REPLY,
            "1 MB parseable-but-unresolvable path: reply must not echo the guest's text, got {} bytes",
            reply.len()
        );

        // Likewise for a 1 MB bogus value tag inside a `set`.
        let huge_tag = format!(
            r#"{{"kind":"set","id":"1","path":"a","value":{{"t":"{}","v":null}}}}"#,
            "z".repeat(1_000_000)
        );
        let reply = handle_text(&handle, SubscriberId(1), &huge_tag);
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
