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

/// A well-formed tagged value nesting **exactly** `containers` JSON containers.
///
/// A tagged value costs **two** containers per level of its own nesting — the
/// `{"t":"l","v":…}` object and the array inside it — so the two halves of the
/// depth boundary pair are built from one generator rather than written out.
///
/// An odd count ends in `{"t":"n"}` (one container); an even count ends in an
/// empty list (two). Both halves of the boundary pair come from this one
/// generator, so they differ by exactly one container and by nothing else — no
/// bare-bracket soup on one side and a real value on the other.
fn nested_value_wire(containers: usize) -> String {
    let wrappers = (containers - 1) / 2;
    let leaf = if containers % 2 == 0 {
        r#"{"t":"l","v":[]}"#
    } else {
        r#"{"t":"n"}"#
    };
    format!(
        "{}{}{}",
        r#"{"t":"l","v":["#.repeat(wrappers),
        leaf,
        r#"]}"#.repeat(wrappers)
    )
}

/// The deepest `Value` the wire admits, as a value rather than as text.
fn at_limit_value() -> Value {
    let mut v = Value::Null;
    for _ in 0..(duet_codec::MAX_JSON_DEPTH - 1) / 2 {
        v = Value::List(vec![v]);
    }
    // The generator and the encoder must agree, or the pair below is comparing
    // two different shapes and the boundary it claims to pin is fictional.
    assert_eq!(
        super::to_text(&duet_codec::encode_value(&v)),
        nested_value_wire(duet_codec::MAX_JSON_DEPTH),
        "the at-limit value must encode to exactly the at-limit wire text"
    );
    v
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
        // Exactly `duet_codec::MAX_JSON_DEPTH` containers: the deepest document
        // every implementation must ACCEPT. Its partner,
        // `value/nesting/over_limit`, is one container deeper and every
        // implementation must refuse it.
        value_case("value/nesting/at_limit", &at_limit_value()),
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

/// Input refused before any Duet decoder sees it, so the reason is [`BAD_JSON`]
/// rather than any [`duet_codec::CodecError`].
///
/// # The nesting cases, and why there are two of them
///
/// `value/nesting/over_limit` is one container past
/// [`duet_codec::MAX_JSON_DEPTH`] and `value/nesting/at_limit` (an *accept*
/// case) is exactly at it. Together they pin the boundary; separately neither
/// does. The corpus previously carried only a 200-level case named
/// `value/nesting/exceeds_parser_recursion_limit`, and it could not see that the
/// Dart and TypeScript guests enforced 128 where this host stops at 127 — 400
/// containers is refused by every implementation whatever its off-by-one. That
/// divergence shipped, and this pair is what would have caught it.
///
/// `value/nesting/far_over_limit` keeps the deep case, renamed. It is not
/// redundant and it is no longer misnamed:
///
/// - **Renamed**, because the limit is no longer "whatever `serde_json`'s
///   parser happens to do". The host enforces
///   [`duet_codec::MAX_JSON_DEPTH`] itself, before parsing.
/// - **Kept**, because it tests something the boundary pair cannot: at 400
///   containers a *recursive* depth check dies by stack overflow — an abort, not
///   a catchable error — on exactly the input it exists to reject. An
///   implementation that passes the pair with a recursive check still fails
///   here.
///
/// The cross-language note this used to carry is now obsolete in the best way:
/// every implementation states its own limit and refuses past it, so there is no
/// asymmetry left to warn about — only a number all three must keep equal.
fn reject_parser_cases() -> Vec<RejectCase> {
    let far_over = format!(
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
            "value/nesting/over_limit",
            Layer::Value,
            nested_value_wire(duet_codec::MAX_JSON_DEPTH + 1),
            BAD_JSON,
        ),
        reject(
            "value/nesting/far_over_limit",
            Layer::Value,
            far_over,
            BAD_JSON,
        ),
    ]
}
