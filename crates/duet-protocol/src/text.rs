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

use crate::command::{CommandHost, NoCommands};
use crate::message::{Push, RequestId, Response};

/// Serves one guest message and returns the JSON text to send back, with no
/// commands registered.
///
/// Total by construction: malformed input becomes a [`Response::Failed`], so a
/// guest always receives well-formed JSON. [`crate::dispatch()`] is itself
/// infallible, so the only failures possible here are decoding ones.
///
/// `subscriber` is the surface's own, supplied by the host. A `subscriber`
/// field appearing in the message is **ignored** — [`crate::Request`] has no
/// such field, so a guest cannot subscribe as another guest.
///
/// An `invoke` is answered with a `failed` naming the command; see
/// [`crate::NoCommands`]. A host that registers commands calls
/// [`handle_text_with`] instead, and gets identical text for everything else —
/// `both_entry_points_produce_the_same_text_when_no_commands_are_registered`
/// pins that byte for byte.
pub fn handle_text(store: &StoreHandle, subscriber: SubscriberId, text: &str) -> String {
    handle_text_with(store, subscriber, &NoCommands, text)
}

/// Serves one guest message against the store and `commands`, returning the
/// JSON text to send back.
///
/// The text-level counterpart of [`crate::dispatch_with`], and the entry point
/// a transport that serves commands should call. Identical to [`handle_text`]
/// for every request kind except `invoke`.
///
/// Still total, and now over a strictly larger surface: a command body is
/// arbitrary user code, and both of the ways it can misbehave that this crate
/// can see — panicking, and returning a value too deep to encode — are turned
/// into a `failed` reply before they reach here. See [`crate::CommandHost`] for
/// which thread a body runs on and the one hazard nothing here can detect.
///
/// # The reply is depth-checked too, not only the request
///
/// The nesting limit guards text arriving *from* a guest. Nothing in the type
/// system stops the host writing a `Value` whose encoding exceeds it, and a
/// `get` for such a value would answer with text no conforming client — the
/// host's own [`crate::decode_response`] included — can parse. Measured before
/// this guard existed: a 200-deep `Value` produced a 3,251-byte reply
/// `serde_json` refused to re-parse.
///
/// [`duet_core::Store::set`] refuses to store a value that deep, which is what
/// makes the situation unreachable through any write. This check is the backstop
/// for the door that leaves open — a `Store` *seeded* over-deep by its embedder,
/// where the value was never written at all. Turning that into a
/// [`Response::Failed`] costs one linear scan of text already produced, and buys
/// the guarantee outright: **`handle_text_with` never returns text it could not
/// decode itself**.
///
/// A command return reaches this check too, and is stopped one layer earlier as
/// well: [`crate::dispatch_with`] refuses an over-deep return outright, because
/// by then the host is holding a `Value` whose *destructor* is the hazard. This
/// scan remains the backstop for text, as it always was.
pub fn handle_text_with(
    store: &StoreHandle,
    subscriber: SubscriberId,
    commands: &dyn CommandHost,
    text: &str,
) -> String {
    let response = match decode(text) {
        Ok(request) => crate::dispatch_with(store, subscriber, commands, request),
        Err((id, message)) => Response::Failed { id, message },
    };
    // `encode_response` produces a plain JSON object; serializing it here is
    // the single encoding step. Note `wry`'s `evaluate_script_with_callback`
    // re-serializes anything a script *returns*, which is why responses are
    // pushed rather than returned — see `duet_webview::response_script`.
    let encoded = serde_json::to_string(&crate::encode_response(&response))
        .unwrap_or_else(|_| FALLBACK_FAILURE.to_string());
    if !duet_codec::exceeds_max_json_depth(&encoded) {
        return encoded;
    }
    // The failure keeps the id the request carried, so the guest fails that one
    // call rather than waiting on a reply that would never arrive.
    let refusal = Response::Failed {
        id: response.id(),
        message: format!(
            "the stored value nests deeper than the {} JSON containers this wire admits",
            duet_codec::MAX_JSON_DEPTH
        ),
    };
    serde_json::to_string(&crate::encode_response(&refusal))
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
/// and the failure carries [`RequestId::UNCORRELATED`] instead.
///
/// Recovering `7` from `"007"` would reintroduce the very mismatch the
/// canonical rule exists to prevent: the reply would name an id the guest never
/// sent, so a guest keying its pending map by the string it sent would hang
/// anyway — while this function's whole purpose is to stop exactly that hang.
/// An id above `duet_codec::MAX_WIRE_ID` is refused for the mirrored reason:
/// echoing it back would send a Dart guest a number its own `int` cannot parse,
/// so the "recovery" would fail on arrival. [`RequestId::UNCORRELATED`] is the honest answer
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
/// document being refused. [`RequestId::UNCORRELATED`] again means "this reply answers no
/// request you can name" — which a guest must handle, see
/// `crates/duet-webview/src/bootstrap.rs`.
fn decode(text: &str) -> Result<crate::Request, (RequestId, String)> {
    if duet_codec::exceeds_max_json_depth(text) {
        return Err((
            RequestId::UNCORRELATED,
            format!(
                "malformed JSON: nests deeper than {} containers",
                duet_codec::MAX_JSON_DEPTH
            ),
        ));
    }
    let json: serde_json::Value = match serde_json::from_str(text) {
        Ok(j) => j,
        Err(e) => return Err((RequestId::UNCORRELATED, format!("malformed JSON: {e}"))),
    };
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(duet_codec::parse_wire_id)
        .map(RequestId)
        .unwrap_or(RequestId::UNCORRELATED);

    crate::decode_request(&json).map_err(|e| (id, e.to_string()))
}

/// Encodes a push as the JSON text a guest receives.
///
/// A webview guest needs this wrapped in JavaScript (see
/// `duet_webview::push_script`); a Flutter guest receives it verbatim on its
/// platform channel, which is why the encoding lives here rather than in a
/// transport crate.
///
/// # Over-deep pushes become `null`, and that is the honest answer
///
/// A push carries no request id, so there is no `failed` reply to send in its
/// place — a guest correlates nothing and can be told nothing. `null` is
/// therefore the only remaining option, and every guest's push handler already
/// drops what it cannot decode. It is still strictly better than emitting text
/// that nests past the wire's limit, which a guest would meet as a parse error
/// on an unrelated code path.
///
/// A notification is the deepest envelope this format has, which is exactly why
/// [`duet_core::MAX_VALUE_DEPTH`] is derived from it: with that bound enforced
/// on the way into the store, no push built from a stored patch can reach this
/// branch.
pub fn push_text(push: &Push) -> String {
    let encoded =
        serde_json::to_string(&crate::encode_push(push)).unwrap_or_else(|_| "null".to_string());
    if duet_codec::exceeds_max_json_depth(&encoded) {
        return "null".to_string();
    }
    encoded
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
    fn both_entry_points_produce_the_same_text_when_no_commands_are_registered() {
        // `handle_text` became a delegation to `handle_text_with` when commands
        // were added, and three transports call it. The claim that has to be
        // earned is that a host with no commands emits the *same bytes* it
        // always did — not merely an equivalent reply. String equality is that
        // claim; anything weaker would let a reordered field or a reworded
        // failure through unnoticed.
        let rt = rt();
        let handle = rt.handle();
        for text in [
            r#"{"kind":"get","id":"1","path":"editor.zoom"}"#,
            r#"{"kind":"get","id":"2","path":"editor.absent"}"#,
            r#"{"kind":"set","id":"3","path":"editor.zoom","value":{"t":"f","v":2.0}}"#,
            r#"{"kind":"set","id":"4","path":"nope.deeper","value":{"t":"n"}}"#,
            r#"{"kind":"unsubscribe","id":"5","subscription":"99"}"#,
            // The malformed cases too: the recovery path is shared, and a
            // regression there is the one that hangs a guest.
            "",
            "not json",
            r#"{"kind":"nope","id":"6"}"#,
            r#"{"kind":"get","id":"007","path":"a"}"#,
            // And `invoke` itself, which is the one kind whose reply is
            // *supposed* to differ once a command host is supplied. With
            // `NoCommands` on both sides it must not.
            r#"{"kind":"invoke","id":"7","command":"add","args":{"t":"m","v":{}}}"#,
        ] {
            assert_eq!(
                handle_text(&handle, SubscriberId(1), text),
                handle_text_with(&handle, SubscriberId(1), &crate::NoCommands, text),
                "{text} must produce identical text through both entry points"
            );
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_invoke_with_no_commands_registered_answers_failed_not_unknown_kind() {
        // What every host shipping today does with an `invoke`, at the text
        // layer a transport actually calls. The kind is `failed` — the message
        // was legal and simply unserved — and the id is echoed, so the guest
        // fails that one call instead of waiting on it.
        let rt = rt();
        let reply = handle_text(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"invoke","id":"9","command":"add","args":{"t":"m","v":{"a":{"t":"i","v":"2"}}}}"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["kind"], "failed", "got {reply}");
        assert_eq!(
            parsed["id"], "9",
            "the refusal must name the request it answers, got {reply}"
        );
        assert!(
            reply.contains("add"),
            "the refusal must name the command, got {reply}"
        );
        // And it is a `Response` a guest can decode, not merely valid JSON.
        crate::decode_response(&parsed).expect("the refusal must decode as a Response");
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_command_answers_over_text_and_can_read_and_write_the_store() {
        // The whole increment, end to end at the text layer: guest text in,
        // command runs, `returned` text out — and the write the command made is
        // visible to Rust afterwards. The shared-state claim from
        // `a_set_request_writes_and_is_visible_to_rust`, now through logic
        // rather than through a bare write.
        use crate::{Args, CommandHost, Outcome};

        struct Zoomer;
        impl CommandHost for Zoomer {
            fn invoke(&self, command: &str, args: Args, store: &StoreHandle) -> Outcome {
                if command != "zoom_by" {
                    return Outcome::Refused(format!(
                        "no command named \"{}\"",
                        duet_core::truncated(&command)
                    ));
                }
                let by = match args.get("by") {
                    Some(Value::Float(by)) => *by,
                    _ => return Outcome::Raised(Value::Str("expected a float \"by\"".into())),
                };
                let path = Path::parse("editor.zoom").expect("test path should parse");
                let current = match store.get(&path) {
                    Ok(Some(Value::Float(v))) => v,
                    other => return Outcome::Raised(Value::Str(format!("unreadable: {other:?}"))),
                };
                match store.set(&path, Value::Float(current + by)) {
                    Ok(()) => Outcome::Returned(Value::Float(current + by)),
                    Err(e) => Outcome::Raised(Value::Str(e.to_string())),
                }
            }
        }

        let rt = rt();
        let handle = rt.handle();
        let reply = handle_text_with(
            &handle,
            SubscriberId(1),
            &Zoomer,
            r#"{"kind":"invoke","id":"1","command":"zoom_by","args":{"t":"m","v":{"by":{"t":"f","v":0.5}}}}"#,
        );
        assert_eq!(
            reply, r#"{"id":"1","kind":"returned","value":{"t":"f","v":1.5}}"#,
            "the reply's exact bytes are the contract the other two languages decode"
        );
        assert_eq!(
            handle
                .get(&Path::parse("editor.zoom").expect("path"))
                .expect("read should succeed"),
            Some(Value::Float(1.5)),
            "a value written by a command must be readable from Rust"
        );

        // An `Err` from the same command is a `raised`, carrying a value rather
        // than prose — the distinction the kind exists for.
        let reply = handle_text_with(
            &handle,
            SubscriberId(1),
            &Zoomer,
            r#"{"kind":"invoke","id":"2","command":"zoom_by","args":{"t":"m","v":{"by":{"t":"s","v":"lots"}}}}"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["kind"], "raised", "got {reply}");

        // And an unknown name is a `failed` — the host would not run it at all.
        let reply = handle_text_with(
            &handle,
            SubscriberId(1),
            &Zoomer,
            r#"{"kind":"invoke","id":"3","command":"drop_database","args":{"t":"m","v":{}}}"#,
        );
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["kind"], "failed", "got {reply}");
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
        // the call hangs anyway — silently. `RequestId::UNCORRELATED` is the honest
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
        // pre-scan answers `RequestId::UNCORRELATED` because reading its id would mean
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

    /// A chain of `n` nested lists around a scalar leaf: `Value::depth() == n`.
    fn chain(n: usize) -> Value {
        let mut v = Value::Int(1);
        for _ in 0..n {
            v = Value::List(vec![v]);
        }
        v
    }

    #[test]
    fn the_value_depth_bound_is_exactly_what_every_envelope_can_carry() {
        // The arithmetic behind `duet_core::MAX_VALUE_DEPTH`, checked against
        // the real encoders in the one crate that can see both constants.
        // Stating 61 in `duet-core` and 127 in `duet-codec` and hoping they
        // stay consistent is how the guests' off-by-one happened in the first
        // place.
        //
        // A `Value` costs two JSON containers per level of its own nesting; the
        // deepest envelope carrying one is a notification push, at three more.
        // So the bound is tested where it binds, and the assertion is
        // decodability rather than length.
        for (depth, must_survive) in [
            (duet_core::MAX_VALUE_DEPTH, true),
            (duet_core::MAX_VALUE_DEPTH + 1, false),
        ] {
            let push = Push::Notification(duet_core::Notification {
                subscriber: SubscriberId(1),
                subscription: duet_core::SubscriptionId(1),
                patch: duet_core::Patch {
                    path: Path::parse("a").expect("path"),
                    value: chain(depth),
                },
            });
            let text = serde_json::to_string(&crate::encode_push(&push)).expect("serializes");
            let within = !duet_codec::exceeds_max_json_depth(&text);
            assert_eq!(
                within,
                must_survive,
                "a value of depth {depth} in a notification push: expected \
                 within-limit={must_survive}, and the limit is \
                 {} containers",
                duet_codec::MAX_JSON_DEPTH
            );
            if must_survive {
                let json: serde_json::Value =
                    serde_json::from_str(&text).expect("must re-parse at the bound");
                crate::decode_push(&json).expect("must re-decode at the bound");
            }
        }
    }

    #[test]
    fn a_value_too_deep_to_encode_cannot_be_written_at_all() {
        // Option (a): the store refuses it, so no `get` can ever be asked to
        // encode one. This is the guarantee that makes the reply unbreakable
        // rather than merely diagnosable.
        let rt = rt();
        let handle = rt.handle();
        let path = Path::parse("editor").expect("path");

        // `editor` is one segment down, so one container already encloses the
        // node being written and the value itself may be one shallower than the
        // bound. Writing the bound's worth of value HERE is already one too
        // deep — the same accounting `Store::set` does.
        assert!(
            handle
                .set(&path, chain(duet_core::MAX_VALUE_DEPTH - 1))
                .is_ok(),
            "a value at exactly the bound must still be writable"
        );
        let refused = handle
            .set(&path, chain(duet_core::MAX_VALUE_DEPTH))
            .expect_err("one past the bound must be refused");
        assert!(
            refused.to_string().contains("nest"),
            "the refusal must say why, got {refused}"
        );
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn a_store_seeded_over_deep_still_answers_with_decodable_text() {
        // Option (b): the backstop for the one door option (a) leaves open —
        // `Store::new`'s seed, which is infallible and so cannot refuse
        // anything. Before this guard, the host wrote a 200-deep value and
        // `get` returned 3,251 bytes that `serde_json` could NOT re-parse; a
        // guest met that as a parse failure or as silence.
        //
        // The assertion is decodability, not length: a reply that happens to be
        // short proves nothing, and a reply that is well-formed JSON but not a
        // `Response` is just as unusable to a guest.
        let rt = Runtime::spawn(Value::map([("editor", chain(200))]), NullSink);
        let reply = handle_text(
            &rt.handle(),
            SubscriberId(1),
            r#"{"kind":"get","id":"7","path":"editor"}"#,
        );

        let json: serde_json::Value = serde_json::from_str(&reply)
            .unwrap_or_else(|e| panic!("the reply must be well-formed JSON: {e}\n{reply}"));
        let decoded = crate::decode_response(&json)
            .unwrap_or_else(|e| panic!("the reply must be decodable by a guest: {e}\n{reply}"));

        match decoded {
            Response::Failed { id, message } => {
                assert_eq!(
                    id,
                    RequestId(7),
                    "the failure must name the request it answers, or the guest \
                     cannot fail that one call"
                );
                assert!(
                    message.contains(&duet_codec::MAX_JSON_DEPTH.to_string()),
                    "the failure must name the limit it hit, got {message}"
                );
            }
            other => panic!("expected a failed response, got {other:?}"),
        }
        rt.shutdown().expect("shutdown should succeed");
    }

    #[test]
    fn an_over_deep_push_becomes_null_rather_than_unparseable_text() {
        // A push carries no id, so there is no `failed` to send instead — but
        // emitting text past the wire's limit would surface in a guest as a
        // parse error on an unrelated code path. `null` is what every guest's
        // push handler already drops.
        let note = duet_core::Notification {
            subscriber: SubscriberId(1),
            subscription: duet_core::SubscriptionId(1),
            patch: duet_core::Patch {
                path: Path::parse("a").expect("path"),
                value: chain(200),
            },
        };
        assert_eq!(push_text(&Push::Notification(note)), "null");

        // And a push within the bound is untouched.
        let ok = duet_core::Notification {
            subscriber: SubscriberId(1),
            subscription: duet_core::SubscriptionId(1),
            patch: duet_core::Patch {
                path: Path::parse("a").expect("path"),
                value: chain(duet_core::MAX_VALUE_DEPTH),
            },
        };
        let text = push_text(&Push::Notification(ok));
        let json: serde_json::Value = serde_json::from_str(&text).expect("well-formed JSON");
        crate::decode_push(&json).expect("a push at the bound must be decodable");
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

        // A 1 MB command name. `command` is the newest guest-controlled string
        // on this wire and the one most likely to be echoed: every reason an
        // `invoke` can be refused — no commands registered, no command by that
        // name, a body that panicked — wants to name the command in its
        // message, and naming it verbatim is exactly the amplifier the two
        // cases above already found for `path` and for a value tag.
        //
        // Deliberately written so it holds on BOTH sides of the increment that
        // adds `invoke`. Before it, this is an unknown `kind` and the 1 MB
        // string never gets read at all; after it, the request decodes and the
        // refusal names the command — bounded, via `duet_core::truncated`. The
        // assertion is the same either way, because the property is the same
        // either way: a `failed` reply whose size does not track the guest's.
        let huge_command = format!(
            r#"{{"kind":"invoke","id":"1","command":"{}","args":{{"t":"m","v":{{}}}}}}"#,
            "c".repeat(1_000_000)
        );
        let reply = handle_text(&handle, SubscriberId(1), &huge_command);
        let parsed: serde_json::Value =
            serde_json::from_str(&reply).expect("1 MB command name: reply must be valid JSON");
        assert_eq!(parsed["kind"], "failed", "1 MB command name: got {reply}");
        assert!(
            reply.len() < MAX_REASONABLE_REPLY,
            "1 MB command name: reply must not echo the guest's text, got {} bytes",
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
