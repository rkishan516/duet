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
    all.extend(invoke_cases());
    all.extend(command_answer_cases());
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

/// A `Value` nesting exactly `containers` of its own levels around a scalar.
///
/// `Value::depth()` counts containers and a scalar leaf is 0, so this is a value
/// of depth exactly `containers`. Encoded it costs `2 * containers + 1` JSON
/// containers — two per level (the `{"t":"l",…}` object and its array) plus the
/// leaf's own tagged object.
fn chain(containers: usize) -> Value {
    let mut v = Value::Int(1);
    for _ in 0..containers {
        v = Value::List(vec![v]);
    }
    v
}

/// An `invoke` carrying one argument nested `containers` deep.
fn invoke_carrying_chain(id: u64, containers: usize) -> Request {
    Request::Invoke {
        id: RequestId(id),
        command: "measure".to_string(),
        args: duet_protocol::Args::from([("shape".to_string(), chain(containers))]),
    }
}

/// Every `invoke` shape, including the deepest argument one may carry.
///
/// # The depth boundary is the same knot the notification push already ties
///
/// An `invoke` envelope nests its arguments **three** containers down — the
/// envelope object, the `args` tagged object, and that object's `"v"` map — which
/// is exactly what a `notification` costs a patch value. So the deepest argument
/// this wire admits is the same [`duet_core::MAX_VALUE_DEPTH`] the store already
/// enforces on every write:
///
/// ```text
/// 2d + 1 + 3 <= 127   =>   d <= 61.5   =>   MAX_VALUE_DEPTH = 61
/// ```
///
/// That is worth stating because the alternative would have been a *second*
/// number: had an `invoke` nested its arguments one container deeper than a push
/// does, a command could accept an argument the store could not hold, or refuse
/// one it could. The pair below pins the boundary rather than trusting the
/// arithmetic — `invoke_argument_at_value_depth_limit` must decode and
/// `envelope/request/invoke_argument_past_value_depth_limit` must not.
fn invoke_cases() -> Vec<AcceptCase> {
    // The arithmetic above, measured against the scan that actually enforces it
    // at the host's front door. Without this the boundary pair below could sit
    // anywhere and still look like a boundary.
    assert!(
        !duet_codec::exceeds_max_json_depth(&super::to_text(&duet_protocol::encode_request(
            &invoke_carrying_chain(1, duet_core::MAX_VALUE_DEPTH)
        ))),
        "an argument at MAX_VALUE_DEPTH must fit inside an invoke envelope"
    );
    assert!(
        duet_codec::exceeds_max_json_depth(&super::to_text(&duet_protocol::encode_request(
            &invoke_carrying_chain(1, duet_core::MAX_VALUE_DEPTH + 1)
        ))),
        "one level past MAX_VALUE_DEPTH must not fit, or the boundary is elsewhere"
    );

    vec![
        // A command that only reads state takes nothing. An empty map must stay
        // an empty map rather than becoming absent — the reject list below has
        // `invoke_without_args` for what absent means.
        request_case(
            "envelope/request/invoke_no_args",
            &Request::Invoke {
                id: RequestId(50),
                command: "documents.reset".to_string(),
                args: duet_protocol::Args::new(),
            },
        ),
        // Several arguments of assorted shapes, inserted out of order so the
        // encoding has to sort them: `args` is a `BTreeMap` precisely so an
        // encoded `invoke` is byte-stable across runs and languages, and a guest
        // whose encoder emitted insertion order would fail this case.
        request_case(
            "envelope/request/invoke_with_args",
            &Request::Invoke {
                id: RequestId(51),
                command: "documents.rename".to_string(),
                args: duet_protocol::Args::from([
                    ("title".to_string(), Value::Str("café ✓".into())),
                    ("id".to_string(), Value::Int(9_007_199_254_740_993)),
                    ("force".to_string(), Value::Bool(true)),
                    ("blob".to_string(), Value::Bytes(vec![0x00, 0x7f, 0xff])),
                    (
                        "tags".to_string(),
                        Value::List(vec![Value::Str("a".into())]),
                    ),
                    (
                        "options".to_string(),
                        Value::map([("dry_run", Value::Bool(false))]),
                    ),
                    ("absent".to_string(), Value::Null),
                ]),
            },
        ),
        // The deepest argument the wire admits. See this function's docs.
        request_case(
            "envelope/request/invoke_argument_at_value_depth_limit",
            &invoke_carrying_chain(52, duet_core::MAX_VALUE_DEPTH),
        ),
    ]
}

/// The two answers an `invoke` has that a `failed` cannot express.
///
/// `returned` carries every interesting value shape, because a command's return
/// is the one place a `Value` reaches a guest without ever having passed through
/// the store — so nothing else in this corpus covers that path.
fn command_answer_cases() -> Vec<AcceptCase> {
    let returned = |name: &str, id: u64, v: Value| {
        response_case(
            name,
            &Response::Returned {
                id: RequestId(id),
                value: v,
            },
        )
    };
    vec![
        // A command with no result. `{"t":"n"}`, never bare JSON `null` — the
        // reject list pins the other half of that rule.
        returned("envelope/response/returned_unit", 53, Value::Null),
        returned("envelope/response/returned_int", 54, Value::Int(i64::MIN)),
        returned("envelope/response/returned_float", 55, Value::Float(0.1)),
        returned(
            "envelope/response/returned_str",
            56,
            Value::Str("café ✓ 😀".into()),
        ),
        returned(
            "envelope/response/returned_bytes",
            57,
            Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        ),
        returned(
            "envelope/response/returned_list",
            58,
            Value::List(vec![Value::Int(1), Value::Null]),
        ),
        returned(
            "envelope/response/returned_map",
            59,
            Value::map([("ok", Value::Bool(true)), ("count", Value::Int(2))]),
        ),
        // The shape a typed error actually has: a struct, which survives as a
        // value the guest decodes back into its own error type. This is the
        // whole reason `raised` is not a `failed`.
        response_case(
            "envelope/response/raised_map",
            &Response::Raised {
                id: RequestId(60),
                error: Value::map([
                    ("code", Value::Str("insufficient_funds".into())),
                    ("short_by", Value::Int(250)),
                ]),
            },
        ),
        // A unit error, and the mirror of `returned_unit`: `{"t":"n"}` is a
        // legal error payload on this field too, and a guest that collapsed it
        // into "no error" would report success for a failed command.
        response_case(
            "envelope/response/raised_unit",
            &Response::Raised {
                id: RequestId(61),
                error: Value::Null,
            },
        ),
    ]
}

/// Every reject case, in a stable order.
pub fn reject_cases() -> Vec<RejectCase> {
    let mut all = Vec::new();
    all.extend(reject_id_cases());
    all.extend(reject_value_cases());
    all.extend(reject_surrogate_cases());
    all.extend(reject_envelope_cases());
    all.extend(reject_command_cases());
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

/// Unpaired UTF-16 surrogates, in every field that carries text.
///
/// # Why these are here
///
/// A lone surrogate is not a character: it has **no UTF-8 encoding at all**.
/// `serde_json` refuses `"\ud800"` outright, so a Rust `String` can never hold
/// one — but Dart and JavaScript strings are sequences of UTF-16 code units and
/// both `jsonDecode` and `JSON.parse` accept the escape happily (measured on
/// Dart 3 and Node v24). Encoding the result to UTF-8 then substitutes U+FFFD
/// **silently**, with no error on either side.
///
/// That makes this the worst shape of divergence the corpus can catch: not a
/// message rejected in one language and accepted in another, but a *value* that
/// changes on the way across while every layer reports success.
///
/// Every text-carrying field gets a case, because the check has to be on the
/// walk rather than on one field: a decoder that validated `Str` payloads and
/// not map keys would pass a single-case corpus and still corrupt data.
fn reject_surrogate_cases() -> Vec<RejectCase> {
    vec![
        reject(
            "value/str/lone_high_surrogate",
            Layer::Value,
            r#"{"t":"s","v":"\ud800"}"#,
            BAD_JSON,
        ),
        // The mirrored case: a low surrogate with nothing before it. A scan
        // that only looked ahead from a high surrogate would miss this one.
        reject(
            "value/str/lone_low_surrogate",
            Layer::Value,
            r#"{"t":"s","v":"\udc00"}"#,
            BAD_JSON,
        ),
        // A high surrogate followed by an ordinary character, rather than by
        // the low surrogate that would complete the pair.
        reject(
            "value/str/high_surrogate_then_text",
            Layer::Value,
            r#"{"t":"s","v":"\ud800a"}"#,
            BAD_JSON,
        ),
        reject(
            "value/map/lone_surrogate_key",
            Layer::Value,
            r#"{"t":"m","v":{"\ud800":{"t":"n"}}}"#,
            BAD_JSON,
        ),
        // In the envelope, not just in a value: `path` is text a guest chooses.
        reject(
            "envelope/path/lone_surrogate",
            Layer::Request,
            r#"{"kind":"get","id":"1","path":"\ud800"}"#,
            BAD_JSON,
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

/// Malformed `invoke`s, and malformed answers to one.
///
/// # Why `args` gets four cases and not one
///
/// [`duet_protocol::Args`] is a `BTreeMap`, not a `Value`, and the narrowing
/// that makes it one lives in the decoder. Each of the three positive-looking
/// shapes below is refused by a *different* step of that narrowing — the field
/// is absent, the tagged value decodes but is not a map, the object is not a
/// tagged value at all — and an implementation that skipped any one of them
/// would hand a command body something it cannot look a field up on, two layers
/// away from any request id to report against.
///
/// # And why `returned`/`raised` get three
///
/// Both fields carry a *required* tagged value. Bare JSON `null` already means
/// "the path is absent" elsewhere in this format, so accepting it here would
/// give one token two meanings on one wire; a unit return is `{"t":"n"}`, which
/// the accept list pins. The absent-field cases are the mirror: a `returned`
/// with no `value` is not a unit return, it is a malformed message.
fn reject_command_cases() -> Vec<RejectCase> {
    let over_deep = super::to_text(&duet_protocol::encode_request(&invoke_carrying_chain(
        1,
        duet_core::MAX_VALUE_DEPTH + 1,
    )));
    vec![
        reject(
            "envelope/request/invoke_without_args",
            Layer::Request,
            r#"{"kind":"invoke","id":"1","command":"add"}"#,
            "bad_shape",
        ),
        // A perfectly well-formed *tagged value* — `decode_value` accepts it —
        // that is not a map. Only the narrowing step stops it.
        reject(
            "envelope/request/invoke_args_tagged_int",
            Layer::Request,
            r#"{"kind":"invoke","id":"1","command":"add","args":{"t":"i","v":"1"}}"#,
            "bad_shape",
        ),
        // The mistake a guest hand-building a request actually makes: a bare
        // JSON object of tagged values, with no `{"t":"m","v":…}` around it.
        // Refused by the tagged-value decoder for its missing `"t"`, before the
        // narrowing step is even reached.
        reject(
            "envelope/request/invoke_args_bare_json_object",
            Layer::Request,
            r#"{"kind":"invoke","id":"1","command":"add","args":{"a":{"t":"i","v":"1"}}}"#,
            "bad_shape",
        ),
        reject(
            "envelope/request/invoke_without_command",
            Layer::Request,
            r#"{"kind":"invoke","id":"1","args":{"t":"m","v":{}}}"#,
            "bad_shape",
        ),
        reject(
            "envelope/request/invoke_command_not_a_string",
            Layer::Request,
            r#"{"kind":"invoke","id":"1","command":7,"args":{"t":"m","v":{}}}"#,
            "bad_shape",
        ),
        // The id rule holds on this envelope like every other. Its own case
        // rather than trust in a shared helper: the id is read before the kind
        // is dispatched on, and an implementation that read them the other way
        // round would report an unknown kind here instead.
        reject(
            "envelope/request/invoke_id_non_canonical",
            Layer::Request,
            r#"{"kind":"invoke","id":"007","command":"add","args":{"t":"m","v":{}}}"#,
            "bad_int",
        ),
        reject(
            "envelope/response/returned_bare_null",
            Layer::Response,
            r#"{"kind":"returned","id":"1","value":null}"#,
            "bad_shape",
        ),
        reject(
            "envelope/response/returned_without_value",
            Layer::Response,
            r#"{"kind":"returned","id":"1"}"#,
            "bad_shape",
        ),
        reject(
            "envelope/response/raised_bare_null",
            Layer::Response,
            r#"{"kind":"raised","id":"1","error":null}"#,
            "bad_shape",
        ),
        reject(
            "envelope/response/raised_without_error",
            Layer::Response,
            r#"{"kind":"raised","id":"1"}"#,
            "bad_shape",
        ),
        // One container past what an `invoke` envelope can carry. The partner of
        // `envelope/request/invoke_argument_at_value_depth_limit`; together they
        // pin the boundary, and separately neither does.
        reject(
            "envelope/request/invoke_argument_past_value_depth_limit",
            Layer::Request,
            over_deep,
            BAD_JSON,
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
