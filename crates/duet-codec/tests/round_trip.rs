//! The invariant the whole crate exists to provide: decode(encode(v)) == v.

use duet_codec::{decode_value, encode_value};
use duet_core::Value;

/// Every structurally distinct `Value` shape, built by enumeration rather than
/// hand-listing. A hand-written list omits exactly the case that breaks.
fn corpus() -> Vec<Value> {
    let scalars = vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(i64::MAX),
        Value::Int(i64::MIN),
        Value::Int(9_007_199_254_740_993), // 2^53 + 1, unsafe in JS as a number
        Value::Float(0.0),
        // WARNING: the `assert_eq!` in the round-trip tests below CANNOT see
        // whether this one keeps its sign. `Value` derives `PartialEq`, so
        // `Float(-0.0) == Float(0.0)` is true — the corpus contained this
        // value the whole time the encoder was dropping its sign in
        // JavaScript. `negative_zero_survives_the_text_hop_with_its_sign`
        // below is the assertion that actually holds it.
        Value::Float(-0.0),
        Value::Float(1.5),
        Value::Float(f64::MIN),
        Value::Float(f64::MAX),
        Value::Float(f64::EPSILON),
        // Full-precision floats whose 17-significant-digit decimal text does
        // not round-trip through serde_json's *default* float parser (it is
        // best-effort, not correctly-rounded). `0.0`/`1.5`/`MIN`/`MAX`/
        // `EPSILON` above all happen to have short decimal forms that survive
        // regardless, so none of them could have caught this — the corpus
        // must include values that actually exercise the lossy path.
        Value::Float(-1.7230175163494897e-48),
        Value::Float(f64::from_bits(0x7F6C_280B_EAA8_E3E7)),
        Value::Float(f64::from_bits(0xEC83_972C_97B6_678E)),
        Value::Float(f64::from_bits(0x0CF9_1633_BE73_28C1)),
        Value::Float(f64::from_bits(0xBFC9_E24F_766F_3ABF)),
        Value::Str(String::new()),
        Value::Str("hello".into()),
        Value::Str("café 🦀 \u{202e}".into()), // multi-byte, emoji, RTL override
        Value::Str("\"quotes\" and \\backslashes\\".into()),
        Value::Bytes(Vec::new()),
        Value::Bytes(vec![0]),
        Value::Bytes((0u8..=255).collect()),
    ];

    let mut all = scalars.clone();
    // One level of nesting over every scalar, in both container kinds.
    all.push(Value::List(scalars.clone()));
    all.push(Value::Map(
        scalars
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("k{i}"), v.clone()))
            .collect(),
    ));
    // Two levels, to catch a decoder that only recurses once.
    all.push(Value::List(vec![Value::List(scalars.clone())]));
    all.push(Value::map([(
        "outer",
        Value::map([("inner", Value::Int(7))]),
    )]));
    // Empty containers — distinct from absent, and a common off-by-one.
    all.push(Value::List(Vec::new()));
    all.push(Value::map([]));
    all
}

#[test]
fn every_value_round_trips_exactly() {
    let corpus = corpus();
    assert_eq!(corpus.len(), 32, "corpus size changed; update deliberately");

    for original in &corpus {
        let encoded = encode_value(original);
        let decoded = decode_value(&encoded).unwrap_or_else(|e| {
            panic!("decode failed for {original:?}: {e}");
        });
        assert_eq!(
            &decoded, original,
            "round trip changed the value\n  encoded as: {encoded}"
        );
    }
}

#[test]
fn round_trip_survives_a_serialized_text_hop() {
    // The real path is Rust -> JSON text -> guest -> JSON text -> Rust, not
    // Rust -> serde_json::Value -> Rust. Serializing to a string and back
    // exercises number formatting, escaping and precision, which the in-memory
    // round trip skips entirely.
    for original in corpus() {
        let text = serde_json::to_string(&encode_value(&original)).expect("encodes to text");
        let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses back");
        let decoded = decode_value(&reparsed).unwrap_or_else(|e| {
            panic!("decode failed for {original:?} via text {text}: {e}");
        });
        assert_eq!(
            &decoded, &original,
            "text round trip changed the value: {text}"
        );
    }
}

#[test]
fn nan_round_trips_through_text_as_nan() {
    // Cannot go in the corpus: NaN != NaN, so assert_eq! would fail on a
    // correct implementation. That non-reflexivity is documented on
    // Value::Float.
    let text = serde_json::to_string(&encode_value(&Value::Float(f64::NAN))).expect("encodes");
    let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
    match decode_value(&reparsed).expect("decodes") {
        Value::Float(f) => assert!(f.is_nan(), "expected NaN, got {f}"),
        other => panic!("NaN must remain a Float, got {other:?}"),
    }
}

#[test]
fn negative_zero_survives_the_text_hop_with_its_sign() {
    // The corpus above holds `Float(-0.0)`, but `assert_eq!` cannot check it:
    // `-0.0 == 0.0` is true, so the sign could vanish with every existing test
    // still green. This compares bits instead.
    //
    // The sign matters observably — `1.0 / -0.0` is −∞ while `1.0 / 0.0` is
    // +∞ — and JavaScript cannot spell negative zero as a JSON number
    // (`JSON.stringify(-0)` is `"0"`), which is why it is encoded as the
    // `"-0"` sentinel rather than a number.
    let text = serde_json::to_string(&encode_value(&Value::Float(-0.0))).expect("encodes");
    assert_eq!(
        text, r#"{"t":"f","v":"-0"}"#,
        "negative zero must encode as the sentinel a JS guest can also emit"
    );
    let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
    match decode_value(&reparsed).expect("decodes") {
        Value::Float(f) => assert_eq!(
            f.to_bits(),
            (-0.0f64).to_bits(),
            "negative zero lost its sign through the text hop: got bits {:#018x}",
            f.to_bits()
        ),
        other => panic!("negative zero must stay a Float, got {other:?}"),
    }

    // The control: positive zero must not acquire a sign.
    let text = serde_json::to_string(&encode_value(&Value::Float(0.0))).expect("encodes");
    assert_eq!(text, r#"{"t":"f","v":0.0}"#);
    let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
    match decode_value(&reparsed).expect("decodes") {
        Value::Float(f) => assert_eq!(f.to_bits(), 0.0f64.to_bits()),
        other => panic!("positive zero must stay a Float, got {other:?}"),
    }
}

#[test]
fn every_float_survives_the_text_hop_bit_exactly() {
    // serde_json's default float parser is best-effort, not correctly-rounded.
    // The `float_roundtrip` feature in Cargo.toml is what makes this pass;
    // without it roughly 30% of finite f64 values change bits here.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut checked = 0usize;
    for _ in 0..20_000 {
        // xorshift64 — deterministic, no rand dependency.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let f = f64::from_bits(state);
        if !f.is_finite() {
            continue;
        }
        let original = Value::Float(f);
        let text = serde_json::to_string(&encode_value(&original)).expect("encodes");
        let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
        match decode_value(&reparsed).expect("decodes") {
            Value::Float(back) => assert_eq!(
                back.to_bits(),
                f.to_bits(),
                "float changed bits through the text hop: {f} -> {back} (text {text})"
            ),
            other => panic!("Float must stay a Float, got {other:?}"),
        }
        checked += 1;
    }
    assert!(
        checked > 15_000,
        "expected most samples to be finite, got {checked}"
    );
}

#[test]
fn decode_never_panics_on_arbitrary_json() {
    // Exhaustive over short JSON-ish strings. The property that matters for a
    // decoder facing untrusted guest input is not that it accepts the right
    // things — it is that it never crashes on the wrong ones.
    const ALPHABET: [char; 8] = ['{', '}', '"', 't', ':', '1', '[', ']'];
    let mut parsed_ok = 0usize;
    let mut checked = 0usize;

    for len in 0..=5usize {
        for mut code in 0..ALPHABET.len().pow(len as u32) {
            let candidate: String = (0..len)
                .map(|_| {
                    let c = ALPHABET[code % ALPHABET.len()];
                    code /= ALPHABET.len();
                    c
                })
                .collect();
            checked += 1;
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&candidate) {
                parsed_ok += 1;
                // Must not panic. Either outcome is fine.
                let _ = decode_value(&json);
            }
        }
    }

    // 8^0 + 8^1 + ... + 8^5 = 37449
    assert_eq!(checked, 37_449, "enumeration changed; update deliberately");
    assert!(
        parsed_ok > 0,
        "the alphabet must produce some valid JSON or this test proves nothing"
    );
}
