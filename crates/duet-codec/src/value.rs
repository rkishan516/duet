//! `duet_core::Value` to and from tagged `serde_json::Value`.

use std::collections::BTreeMap;

use duet_core::Value;
use serde_json::{Map as JsonMap, Value as Json};

use crate::base64;
use crate::error::CodecError;

/// Builds a `{"t":tag,"v":payload}` object.
fn tagged(tag: &str, payload: Json) -> Json {
    let mut m = JsonMap::new();
    m.insert("t".to_string(), Json::String(tag.to_string()));
    m.insert("v".to_string(), payload);
    Json::Object(m)
}

/// Encodes a [`Value`] into its tagged JSON form.
///
/// Total: every `Value` has an encoding, including non-finite floats.
pub(crate) fn encode_value(value: &Value) -> Json {
    match value {
        Value::Null => {
            let mut m = JsonMap::new();
            m.insert("t".to_string(), Json::String("n".to_string()));
            Json::Object(m)
        }
        Value::Bool(b) => tagged("bool", Json::Bool(*b)),
        // A string, so an i64 above 2^53 survives the JavaScript side intact.
        Value::Int(i) => tagged("i", Json::String(i.to_string())),
        Value::Float(f) => tagged("f", encode_float(*f)),
        Value::Str(s) => tagged("s", Json::String(s.clone())),
        Value::Bytes(b) => tagged("b", Json::String(base64::encode(b))),
        Value::List(items) => tagged("l", Json::Array(items.iter().map(encode_value).collect())),
        Value::Map(entries) => {
            let mut m = JsonMap::new();
            for (k, v) in entries {
                m.insert(k.clone(), encode_value(v));
            }
            tagged("m", Json::Object(m))
        }
    }
}

/// JSON has no representation for non-finite floats, so they travel as string
/// sentinels. Without this they would decode back as `Null`, changing the
/// value's variant rather than its magnitude.
fn encode_float(f: f64) -> Json {
    if f.is_nan() {
        return Json::String("NaN".to_string());
    }
    if f == f64::INFINITY {
        return Json::String("Infinity".to_string());
    }
    if f == f64::NEG_INFINITY {
        return Json::String("-Infinity".to_string());
    }
    serde_json::Number::from_f64(f)
        .map(Json::Number)
        .unwrap_or_else(|| Json::String("NaN".to_string()))
}

/// Decodes a tagged JSON value.
///
/// # Errors
///
/// Returns a [`CodecError`] describing the first structural problem found.
/// Total over all JSON input: never panics, whatever a guest sends.
pub(crate) fn decode_value(json: &Json) -> Result<Value, CodecError> {
    let obj = json
        .as_object()
        .ok_or_else(|| CodecError::BadShape(format!("expected an object, found {json}")))?;
    let tag = obj
        .get("t")
        .ok_or_else(|| CodecError::BadShape("missing \"t\"".to_string()))?
        .as_str()
        .ok_or_else(|| CodecError::BadShape("\"t\" must be a string".to_string()))?;

    if tag == "n" {
        return Ok(Value::Null);
    }

    let payload = obj
        .get("v")
        .ok_or_else(|| CodecError::BadShape(format!("tag \"{tag}\" requires \"v\"")))?;

    match tag {
        "bool" => payload
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| CodecError::BadShape("\"bool\" payload must be a boolean".to_string())),
        "i" => {
            let s = payload.as_str().ok_or_else(|| {
                CodecError::BadInt("payload must be a decimal string".to_string())
            })?;
            s.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| CodecError::BadInt(s.to_string()))
        }
        "f" => decode_float(payload),
        "s" => payload
            .as_str()
            .map(|s| Value::Str(s.to_string()))
            .ok_or_else(|| CodecError::BadShape("\"s\" payload must be a string".to_string())),
        "b" => {
            let s = payload
                .as_str()
                .ok_or_else(|| CodecError::BadBase64("payload must be a string".to_string()))?;
            base64::decode(s).map(Value::Bytes)
        }
        "l" => {
            let arr = payload.as_array().ok_or_else(|| {
                CodecError::BadShape("\"l\" payload must be an array".to_string())
            })?;
            arr.iter()
                .map(decode_value)
                .collect::<Result<_, _>>()
                .map(Value::List)
        }
        "m" => {
            let obj = payload.as_object().ok_or_else(|| {
                CodecError::BadShape("\"m\" payload must be an object".to_string())
            })?;
            let mut out = BTreeMap::new();
            for (k, v) in obj {
                out.insert(k.clone(), decode_value(v)?);
            }
            Ok(Value::Map(out))
        }
        other => Err(CodecError::UnknownTag(other.to_string())),
    }
}

fn decode_float(payload: &Json) -> Result<Value, CodecError> {
    if let Some(n) = payload.as_f64() {
        return Ok(Value::Float(n));
    }
    match payload.as_str() {
        Some("NaN") => Ok(Value::Float(f64::NAN)),
        Some("Infinity") => Ok(Value::Float(f64::INFINITY)),
        Some("-Infinity") => Ok(Value::Float(f64::NEG_INFINITY)),
        Some(other) => Err(CodecError::BadFloat(other.to_string())),
        None => Err(CodecError::BadFloat(
            "payload must be a number or a sentinel string".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duet_core::Value;

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("test JSON should parse")
    }

    #[test]
    fn encodes_each_variant_with_its_tag() {
        assert_eq!(encode_value(&Value::Null), json(r#"{"t":"n"}"#));
        assert_eq!(
            encode_value(&Value::Bool(true)),
            json(r#"{"t":"bool","v":true}"#)
        );
        assert_eq!(encode_value(&Value::Int(42)), json(r#"{"t":"i","v":"42"}"#));
        assert_eq!(
            encode_value(&Value::Float(1.5)),
            json(r#"{"t":"f","v":1.5}"#)
        );
        assert_eq!(
            encode_value(&Value::Str("hi".into())),
            json(r#"{"t":"s","v":"hi"}"#)
        );
        assert_eq!(
            encode_value(&Value::Bytes(b"foo".to_vec())),
            json(r#"{"t":"b","v":"Zm9v"}"#)
        );
        assert_eq!(
            encode_value(&Value::List(vec![Value::Int(1)])),
            json(r#"{"t":"l","v":[{"t":"i","v":"1"}]}"#)
        );
        assert_eq!(
            encode_value(&Value::map([("k", Value::Bool(false))])),
            json(r#"{"t":"m","v":{"k":{"t":"bool","v":false}}}"#)
        );
    }

    #[test]
    fn int_is_a_string_so_both_guests_agree() {
        // JavaScript numbers are IEEE-754 doubles. Encoded as a JSON number,
        // this value would arrive intact in Dart and corrupted in the webview.
        let big = Value::Int(9_007_199_254_740_993); // 2^53 + 1
        assert_eq!(
            encode_value(&big),
            json(r#"{"t":"i","v":"9007199254740993"}"#)
        );
        assert_eq!(decode_value(&encode_value(&big)).expect("decodes"), big);
    }

    #[test]
    fn non_finite_floats_use_string_sentinels() {
        // Documented on Value::Float as a known hazard: NaN has no JSON form,
        // so an untagged encoding decodes it back as Null — changing the
        // variant, not just the magnitude. These sentinels are the fix.
        assert_eq!(
            encode_value(&Value::Float(f64::NAN)),
            json(r#"{"t":"f","v":"NaN"}"#)
        );
        assert_eq!(
            encode_value(&Value::Float(f64::INFINITY)),
            json(r#"{"t":"f","v":"Infinity"}"#)
        );
        assert_eq!(
            encode_value(&Value::Float(f64::NEG_INFINITY)),
            json(r#"{"t":"f","v":"-Infinity"}"#)
        );
    }

    #[test]
    fn nan_round_trips_as_nan_not_null() {
        let decoded = decode_value(&encode_value(&Value::Float(f64::NAN))).expect("decodes");
        match decoded {
            Value::Float(f) => assert!(f.is_nan(), "must still be NaN, got {f}"),
            other => panic!("NaN must stay a Float, got {other:?}"),
        }
        // Note: NaN != NaN, so this cannot be asserted with assert_eq!. That
        // non-reflexivity is documented on Value::Float.
    }

    #[test]
    fn bytes_and_str_stay_distinguishable() {
        // The single clearest reason the encoding is tagged.
        let s = Value::Str("foo".into());
        let b = Value::Bytes(b"foo".to_vec());
        assert_ne!(encode_value(&s), encode_value(&b));
        assert_eq!(decode_value(&encode_value(&s)).expect("decodes"), s);
        assert_eq!(decode_value(&encode_value(&b)).expect("decodes"), b);
    }

    #[test]
    fn int_and_float_stay_distinguishable() {
        let i = Value::Int(1);
        let f = Value::Float(1.0);
        assert_ne!(encode_value(&i), encode_value(&f));
        assert_eq!(decode_value(&encode_value(&i)).expect("decodes"), i);
        assert_eq!(decode_value(&encode_value(&f)).expect("decodes"), f);
    }

    #[test]
    fn decode_rejects_malformed_shapes_without_panicking() {
        for bad in [
            r#"42"#,                                    // not an object
            r#"{}"#,                                    // no tag
            r#"{"t":"q","v":1}"#,                       // unknown tag
            r#"{"t":"i"}"#,                             // missing payload
            r#"{"t":"i","v":42}"#,                      // Int payload must be a string
            r#"{"t":"i","v":"nope"}"#,                  // not a decimal integer
            r#"{"t":"i","v":"999999999999999999999"}"#, // overflows i64
            r#"{"t":"f","v":"huge"}"#,                  // unrecognised float sentinel
            r#"{"t":"f","v":true}"#,                    // float payload neither number nor string
            r#"{"t":"b","v":"!!!"}"#,                   // invalid base64
            r#"{"t":"bool","v":"yes"}"#,                // wrong payload type
            r#"{"t":"l","v":{}}"#,                      // List payload must be an array
            r#"{"t":"m","v":[]}"#,                      // Map payload must be an object
            r#"{"t":5,"v":1}"#,                         // tag must be a string
        ] {
            let parsed = json(bad);
            assert!(
                decode_value(&parsed).is_err(),
                "{bad} must be rejected, got {:?}",
                decode_value(&parsed)
            );
        }
    }

    #[test]
    fn deeply_nested_input_does_not_overflow_the_stack() {
        // A guest can send arbitrarily nested JSON. serde_json has its own
        // recursion limit; this pins that we surface it as an error rather
        // than a crash.
        let deep = format!(
            "{}{}{}",
            r#"{"t":"l","v":["#.repeat(200),
            r#"{"t":"n"}"#,
            r#"]}"#.repeat(200)
        );
        match serde_json::from_str::<serde_json::Value>(&deep) {
            Ok(parsed) => {
                // Parsed fine; decoding must also not panic.
                let _ = decode_value(&parsed);
            }
            Err(_) => {
                // serde_json rejected it first, which is also acceptable.
            }
        }
    }
}
