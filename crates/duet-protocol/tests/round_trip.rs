//! Round-trips through serialized text, plus adversarial input.

use duet_core::{Path, SubscriptionId, Value};
use duet_protocol::{
    Push, Request, RequestId, Response, decode_push, decode_request, decode_response, encode_push,
    encode_request, encode_response,
};

fn p(s: &str) -> Path {
    Path::parse(s).expect("test path should parse")
}

/// Values chosen to break a naive encoder: full-precision floats, an `i64`
/// above 2^53, bytes, and strings needing escaping.
fn awkward_values() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Bool(false),
        Value::Int(9_007_199_254_740_993),
        Value::Int(i64::MIN),
        Value::Float(-1.7230175163494897e-48),
        Value::Float(f64::from_bits(0x4021_2345_6789_ABCD)),
        Value::Str("\"quotes\" \\slashes\\ \n newline café 🦀".into()),
        Value::Bytes((0u8..=255).collect()),
        Value::List(vec![Value::Int(1), Value::Null]),
        Value::map([("k", Value::Float(0.1))]),
    ]
}

#[test]
fn every_request_survives_a_text_hop() {
    let mut checked = 0usize;
    for value in awkward_values() {
        for original in [
            Request::Get {
                id: RequestId(u64::MAX),
                path: p("documents[3].title"),
            },
            Request::Set {
                id: RequestId(1),
                path: p("a.b"),
                value: value.clone(),
            },
            Request::Subscribe {
                id: RequestId(2),
                path: Path::root(),
            },
            Request::Unsubscribe {
                id: RequestId(3),
                subscription: SubscriptionId(u64::MAX),
            },
        ] {
            let text = serde_json::to_string(&encode_request(&original)).expect("encodes");
            let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
            let decoded = decode_request(&reparsed)
                .unwrap_or_else(|e| panic!("decode failed for {original:?} via {text}: {e}"));
            assert_eq!(
                decoded, original,
                "text round trip changed the request: {text}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 40, "enumeration changed; update deliberately");
}

#[test]
fn every_response_survives_a_text_hop() {
    let mut checked = 0usize;
    for value in awkward_values() {
        for original in [
            Response::Value {
                id: RequestId(1),
                value: Some(value.clone()),
            },
            Response::Value {
                id: RequestId(2),
                value: None,
            },
            Response::Done { id: RequestId(3) },
            Response::Subscribed {
                id: RequestId(4),
                subscription: SubscriptionId(7),
                snapshot: Some(value.clone()),
            },
            Response::Failed {
                id: RequestId(5),
                message: "café \"quoted\" 🦀".to_string(),
            },
        ] {
            let text = serde_json::to_string(&encode_response(&original)).expect("encodes");
            let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
            let decoded = decode_response(&reparsed)
                .unwrap_or_else(|e| panic!("decode failed for {original:?} via {text}: {e}"));
            assert_eq!(
                decoded, original,
                "text round trip changed the response: {text}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 50, "enumeration changed; update deliberately");
}

#[test]
fn a_push_survives_a_text_hop() {
    for value in awkward_values() {
        let original = Push::Notification(duet_core::Notification {
            subscriber: duet_core::SubscriberId(u64::MAX),
            subscription: SubscriptionId(1),
            patch: duet_core::Patch {
                path: p("a[0].b"),
                value,
            },
        });
        let text = serde_json::to_string(&encode_push(&original)).expect("encodes");
        let reparsed: serde_json::Value = serde_json::from_str(&text).expect("parses");
        assert_eq!(decode_push(&reparsed).expect("decodes"), original);
    }
}

#[test]
fn an_absent_value_stays_distinct_from_a_null_value() {
    // `Response::Value { value: None }` means the path does not exist;
    // `Some(Value::Null)` means it exists and holds null. Collapsing them
    // would make "missing" and "explicitly null" indistinguishable to a guest.
    let absent = Response::Value {
        id: RequestId(1),
        value: None,
    };
    let null = Response::Value {
        id: RequestId(1),
        value: Some(Value::Null),
    };
    assert_ne!(encode_response(&absent), encode_response(&null));
    assert_eq!(
        decode_response(&encode_response(&absent)).expect("decodes"),
        absent
    );
    assert_eq!(
        decode_response(&encode_response(&null)).expect("decodes"),
        null
    );
}

#[test]
fn decoding_never_panics_on_arbitrary_json() {
    // The property that matters for a decoder facing untrusted guest input is
    // not that it accepts the right things — it is that it never crashes on the
    // wrong ones.
    const ALPHABET: [char; 8] = ['{', '}', '"', 'k', ':', '1', '[', ']'];
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
                let _ = decode_request(&json);
                let _ = decode_response(&json);
                let _ = decode_push(&json);
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
