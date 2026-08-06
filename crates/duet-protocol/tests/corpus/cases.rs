//! The corpus's coverage: every case Rust, Dart and TypeScript must agree on.
//!
//! A gap in this list is a gap in all three languages at once, so the cases are
//! chosen for the divergences that have actually bitten this project — the
//! surrogate-range map key, the negative zero, the non-canonical id, the `i64`
//! past 2^53 — rather than for a tidy-looking cross product.

use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};
use duet_protocol::{Push, Request, RequestId, Response};

use super::{
    AcceptCase, BAD_JSON, Layer, RejectCase, push_case, reject, request_case, response_case,
    value_case, value_spelled_case,
};

fn p(s: &str) -> Path {
    Path::parse(s).unwrap_or_else(|e| panic!("corpus path {s:?} should parse: {e}"))
}

/// Every accept case, in a stable order.
pub fn accept_cases() -> Vec<AcceptCase> {
    let mut all = Vec::new();
    all.extend(scalar_cases());
    all.extend(float_cases());
    all.extend(text_and_bytes_cases());
    all.extend(container_cases());
    all.extend(request_cases());
    all.extend(response_and_push_cases());
    all
}

/// Null, booleans, and the integer domain.
fn scalar_cases() -> Vec<AcceptCase> {
    vec![
        value_case("value/null", &Value::Null),
        value_case("value/bool/true", &Value::Bool(true)),
        value_case("value/bool/false", &Value::Bool(false)),
        value_case("value/int/zero", &Value::Int(0)),
        value_case("value/int/one", &Value::Int(1)),
        value_case("value/int/negative_one", &Value::Int(-1)),
        value_case("value/int/i64_min", &Value::Int(i64::MIN)),
        value_case("value/int/i64_max", &Value::Int(i64::MAX)),
        // 2^53 + 1: exact in Dart and Rust, corrupted by a JavaScript reader
        // that treats the payload as a number rather than a string.
        value_case("value/int/above_2_53", &Value::Int(9_007_199_254_740_993)),
    ]
}

/// Every float that is awkward for *some* language.
fn float_cases() -> Vec<AcceptCase> {
    vec![
        value_case("value/float/zero", &Value::Float(0.0)),
        // `-0.0 == 0.0` is true, so only the witness's bit comparison can see
        // a decoder that drops the sign. `JSON.stringify(-0)` is `"0"`, which
        // is why the wire spells this one as a string sentinel.
        value_case("value/float/negative_zero", &Value::Float(-0.0)),
        value_case("value/float/one", &Value::Float(1.0)),
        value_case("value/float/one_tenth", &Value::Float(0.1)),
        // Rust renders this `1e16`; JavaScript renders the same double
        // `10000000000000000`. Hence `reencode_byte_exact`.
        value_case("value/float/1e16", &Value::Float(1e16)),
        value_case("value/float/max", &Value::Float(f64::MAX)),
        value_case("value/float/subnormal_min", &Value::Float(5e-324)),
        value_case("value/float/nan", &Value::Float(f64::NAN)),
        value_case("value/float/infinity", &Value::Float(f64::INFINITY)),
        value_case(
            "value/float/negative_infinity",
            &Value::Float(f64::NEG_INFINITY),
        ),
        // The decoder is deliberately wider than the encoder: a guest cannot
        // always control the JSON spelling it emits. These pin that width and
        // name the canonical form each normalises to.
        value_spelled_case(
            "value/float/integer_spelling",
            r#"{"t":"f","v":1}"#,
            &Value::Float(1.0),
        ),
        value_spelled_case(
            "value/float/negative_zero_number_spelling",
            r#"{"t":"f","v":-0.0}"#,
            &Value::Float(-0.0),
        ),
    ]
}

/// Strings a naive escaper mangles, and bytes at every base64 padding case.
fn text_and_bytes_cases() -> Vec<AcceptCase> {
    let control: String = (0u32..=0x1F)
        .map(|c| char::from_u32(c).unwrap_or('\u{FFFD}'))
        .collect();
    vec![
        value_case("value/str/empty", &Value::Str(String::new())),
        value_case("value/str/ascii", &Value::Str("hi".into())),
        value_case("value/str/multilingual", &Value::Str("café ✓ 😀".into())),
        value_case("value/str/json_escapes", &Value::Str("a\"b\\c\nd".into())),
        // U+0000..U+001F must all be escaped by every JSON writer, and a raw
        // NUL in the middle of a string is where C-shaped implementations stop.
        value_case("value/str/control_chars", &Value::Str(control)),
        value_case("value/bytes/empty", &Value::Bytes(Vec::new())),
        value_case("value/bytes/foo", &Value::Bytes(b"foo".to_vec())),
        // One at each length mod 3, so all three base64 padding cases appear:
        // no padding, "==" and "=". High and zero bytes exercise the alphabet.
        value_case(
            "value/bytes/len_mod_3_is_0",
            &Value::Bytes(vec![0x00, 0x7f, 0xff]),
        ),
        value_case(
            "value/bytes/len_mod_3_is_1",
            &Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        ),
        value_case(
            "value/bytes/len_mod_3_is_2",
            &Value::Bytes(vec![0x00, 0x01, 0xfe, 0xff, 0x80]),
        ),
    ]
}

/// Containers, including the map that pins code-point key order.
fn container_cases() -> Vec<AcceptCase> {
    let nested = Value::List(vec![
        Value::map([(
            "inner",
            // A float buried three levels down, so `reencode_byte_exact`'s
            // recursion is actually exercised rather than only its top level.
            Value::List(vec![Value::Int(1), Value::Float(0.5), Value::Null]),
        )]),
        Value::Bool(true),
    ]);
    vec![
        value_case("value/list/empty", &Value::List(Vec::new())),
        value_case("value/map/empty", &Value::map([])),
        value_case("value/nested/list_in_map_in_list", &nested),
        // U+1F600 is non-BMP: UTF-16 encodes it as the surrogate pair D83D
        // DE00, and 0xD83D sorts BELOW U+E000. An implementation comparing
        // UTF-16 code units (Dart's String.compareTo, JavaScript's default
        // Array.prototype.sort) puts it first; code-point order puts it last.
        // A BMP-only map would pass with the bug still present.
        value_case(
            "value/map/code_point_order",
            &Value::map([
                ("\u{1F600}", Value::Int(3)),
                ("\u{E000}", Value::Int(1)),
                ("\u{FFFD}", Value::Int(2)),
            ]),
        ),
    ]
}

/// Every [`Request`] variant, plus both ends of the id domain.
fn request_cases() -> Vec<AcceptCase> {
    vec![
        request_case(
            "envelope/request/get",
            &Request::Get {
                id: RequestId(7),
                path: p("editor.zoom"),
            },
        ),
        request_case(
            "envelope/request/get_root",
            &Request::Get {
                id: RequestId(1),
                path: Path::root(),
            },
        ),
        request_case(
            "envelope/request/set",
            &Request::Set {
                id: RequestId(2),
                path: p("documents[3].title"),
                value: Value::Str("hi".into()),
            },
        ),
        request_case(
            "envelope/request/subscribe",
            &Request::Subscribe {
                id: RequestId(3),
                path: p("a.b"),
            },
        ),
        request_case(
            "envelope/request/unsubscribe",
            &Request::Unsubscribe {
                id: RequestId(4),
                subscription: SubscriptionId(9),
            },
        ),
        request_case(
            "envelope/id/zero",
            &Request::Get {
                id: RequestId(0),
                path: p("a"),
            },
        ),
        // The top of the wire's id domain — i64::MAX, not u64::MAX, because
        // Dart's native `int` is 64-bit signed.
        request_case(
            "envelope/id/i64_max",
            &Request::Get {
                id: RequestId(duet_codec::MAX_WIRE_ID),
                path: p("a"),
            },
        ),
    ]
}

/// Every [`Response`] variant, and the push.
fn response_and_push_cases() -> Vec<AcceptCase> {
    let push = Push::Notification(Notification {
        subscriber: SubscriberId(1),
        subscription: SubscriptionId(2),
        patch: Patch {
            path: p("editor.zoom"),
            value: Value::Float(1.5),
        },
    });
    vec![
        response_case(
            "envelope/response/value_present",
            &Response::Value {
                id: RequestId(1),
                value: Some(Value::Int(42)),
            },
        ),
        // JSON `null` means the path is ABSENT; `{"t":"n"}` means it holds
        // Value::Null. A guest that collapsed the two would lose the
        // distinction, so both are pinned here side by side.
        response_case(
            "envelope/response/value_absent",
            &Response::Value {
                id: RequestId(2),
                value: None,
            },
        ),
        response_case(
            "envelope/response/value_holding_null",
            &Response::Value {
                id: RequestId(3),
                value: Some(Value::Null),
            },
        ),
        response_case(
            "envelope/response/done",
            &Response::Done { id: RequestId(4) },
        ),
        response_case(
            "envelope/response/subscribed",
            &Response::Subscribed {
                id: RequestId(5),
                subscription: SubscriptionId(6),
                snapshot: Some(Value::Bool(true)),
            },
        ),
        response_case(
            "envelope/response/subscribed_absent_snapshot",
            &Response::Subscribed {
                id: RequestId(7),
                subscription: SubscriptionId(8),
                snapshot: None,
            },
        ),
        response_case(
            "envelope/response/failed",
            &Response::Failed {
                id: RequestId(9),
                message: "no such path: café".to_string(),
            },
        ),
        push_case("envelope/push/notification", &push),
    ]
}

/// Every reject case, in a stable order.
pub fn reject_cases() -> Vec<RejectCase> {
    let mut all = Vec::new();
    all.extend(reject_id_cases());
    all.extend(reject_value_cases());
    all.extend(reject_envelope_cases());
    all.extend(reject_parser_cases());
    all
}

/// The id rule, in the field where getting it wrong causes a silent hang.
///
/// The host echoes ids back in canonical form. A guest that sent `"007"` and
/// keyed its pending map by that string would never match the `"7"` it gets
/// back — no error, just a promise that never settles.
fn reject_id_cases() -> Vec<RejectCase> {
    let get = |id: &str| format!(r#"{{"kind":"get","id":{id},"path":"a"}}"#);
    vec![
        reject(
            "envelope/id/non_canonical_leading_zero",
            Layer::Request,
            get(r#""007""#),
            "bad_int",
        ),
        reject(
            "envelope/id/leading_plus",
            Layer::Request,
            get(r#""+1""#),
            "bad_int",
        ),
        reject(
            "envelope/id/trailing_space",
            Layer::Request,
            get(r#""1 ""#),
            "bad_int",
        ),
        reject("envelope/id/empty", Layer::Request, get(r#""""#), "bad_int"),
        // A JSON number, not a string: the wrong TYPE, so the shape rule fires
        // before the spelling rule does.
        reject(
            "envelope/id/json_number",
            Layer::Request,
            get("1"),
            "bad_shape",
        ),
        // i64::MAX + 1: canonically spelled, but outside the wire's id domain
        // because Dart's `int.tryParse` returns null for it.
        reject(
            "envelope/id/above_domain",
            Layer::Request,
            get(r#""9223372036854775808""#),
            "bad_int",
        ),
        reject(
            "envelope/response/id_non_canonical",
            Layer::Response,
            r#"{"kind":"done","id":"007"}"#,
            "bad_int",
        ),
    ]
}

/// Malformed tagged values.
fn reject_value_cases() -> Vec<RejectCase> {
    vec![
        reject(
            "value/tag/unknown",
            Layer::Value,
            r#"{"t":"q","v":1}"#,
            "unknown_tag",
        ),
        reject("value/tag/missing", Layer::Value, r#"{}"#, "bad_shape"),
        reject(
            "value/payload/missing",
            Layer::Value,
            r#"{"t":"i"}"#,
            "bad_shape",
        ),
        // An Int payload must be a decimal string; a JSON number would lose
        // precision above 2^53 in a JavaScript guest.
        reject(
            "value/int/json_number",
            Layer::Value,
            r#"{"t":"i","v":42}"#,
            "bad_int",
        ),
        reject(
            "value/int/non_canonical",
            Layer::Value,
            r#"{"t":"i","v":"007"}"#,
            "bad_int",
        ),
        reject(
            "value/float/unknown_sentinel",
            Layer::Value,
            r#"{"t":"f","v":"huge"}"#,
            "bad_float",
        ),
        reject(
            "value/bytes/not_base64",
            Layer::Value,
            r#"{"t":"b","v":"!!!"}"#,
            "bad_base64",
        ),
    ]
}

/// Malformed envelopes.
fn reject_envelope_cases() -> Vec<RejectCase> {
    vec![
        reject(
            "envelope/kind/unknown",
            Layer::Request,
            r#"{"kind":"nope","id":"1"}"#,
            "unknown_tag",
        ),
        reject(
            "envelope/path/unparseable",
            Layer::Request,
            r#"{"kind":"get","id":"1","path":"a.[0]"}"#,
            "bad_path",
        ),
        reject(
            "envelope/request/set_without_value",
            Layer::Request,
            r#"{"kind":"set","id":"1","path":"a"}"#,
            "bad_shape",
        ),
        reject(
            "envelope/push/unknown_kind",
            Layer::Push,
            r#"{"kind":"nope"}"#,
            "unknown_tag",
        ),
    ]
}

/// Input the JSON parser refuses before any Duet decoder sees it.
///
/// # A known cross-language divergence
///
/// `value/nesting/exceeds_parser_recursion_limit` is 200 levels of nested
/// tagged lists — 400 nested containers. `serde_json` has a recursion limit
/// (128 by default) and refuses to parse it, so Rust's reason is `bad_json`,
/// not any [`duet_codec::CodecError`]: the codec's own recursive decoder is
/// never reached. Dart's `jsonDecode` has a comparable guard.
///
/// **A JavaScript guest may not.** V8's `JSON.parse` has no such limit at this
/// depth, so a JS decoder would parse the text and then recurse through it
/// successfully — accepting input the host refuses. That asymmetry is real and
/// this entry is where it surfaces: the JavaScript client must impose its own
/// depth limit to satisfy this case, which is exactly the kind of gap the
/// corpus exists to make visible rather than to paper over.
fn reject_parser_cases() -> Vec<RejectCase> {
    let deep = format!(
        "{}{}{}",
        r#"{"t":"l","v":["#.repeat(200),
        r#"{"t":"n"}"#,
        r#"]}"#.repeat(200)
    );
    vec![
        reject(
            "wire/malformed_json",
            Layer::Request,
            r#"{"kind":"get","id":"#,
            BAD_JSON,
        ),
        reject(
            "value/nesting/exceeds_parser_recursion_limit",
            Layer::Value,
            deep,
            BAD_JSON,
        ),
    ]
}
