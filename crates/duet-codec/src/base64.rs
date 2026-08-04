//! Standard base64 (RFC 4648) with padding, for `Value::Bytes`.

use crate::error::CodecError;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `bytes` as standard base64 with padding.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// Maps a base64 character to its 6-bit value, or `None` if it is not in the
/// alphabet.
fn sextet(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 26),
        b'0'..=b'9' => Some((c - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes standard base64 with padding.
///
/// # Errors
///
/// Returns [`CodecError::BadBase64`] for any input that is not exactly
/// well-formed: wrong length, characters outside the alphabet, or misplaced
/// padding. This decodes untrusted guest input, so it is deliberately strict —
/// it never guesses and never panics.
pub(crate) fn decode(s: &str) -> Result<Vec<u8>, CodecError> {
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(CodecError::BadBase64(format!(
            "length {} is not a multiple of 4",
            bytes.len()
        )));
    }

    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (n, quantum) in bytes.chunks(4).enumerate() {
        let is_last = n == bytes.len() / 4 - 1;
        let pad = quantum.iter().filter(|&&c| c == b'=').count();

        if pad > 0 && !is_last {
            return Err(CodecError::BadBase64(
                "padding may appear only in the final quantum".to_string(),
            ));
        }
        if pad > 2 {
            return Err(CodecError::BadBase64(format!("{pad} padding characters")));
        }
        // Padding must be a suffix: "Zg==" is legal, "Z=g=" is not.
        if pad > 0 && quantum[4 - pad..].iter().any(|&c| c != b'=') {
            return Err(CodecError::BadBase64(
                "padding must be at the end of the quantum".to_string(),
            ));
        }

        let mut acc = 0u32;
        for &c in &quantum[..4 - pad] {
            let v = sextet(c).ok_or_else(|| {
                CodecError::BadBase64(format!("character {:?} is not in the alphabet", c as char))
            })?;
            acc = (acc << 6) | v;
        }
        // Left-align the accumulated bits for the bytes we will emit.
        acc <<= 6 * pad;

        out.push((acc >> 16) as u8);
        if pad < 2 {
            out.push((acc >> 8) as u8);
        }
        if pad < 1 {
            out.push(acc as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_rfc4648_test_vectors() {
        // From RFC 4648 §10. Pinning a published vector set rather than
        // round-tripping our own output, which would pass even if both
        // directions were wrong in the same way.
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn decodes_the_rfc4648_test_vectors() {
        assert_eq!(decode("").expect("empty decodes"), b"");
        assert_eq!(decode("Zg==").expect("decodes"), b"f");
        assert_eq!(decode("Zm8=").expect("decodes"), b"fo");
        assert_eq!(decode("Zm9v").expect("decodes"), b"foo");
        assert_eq!(decode("Zm9vYg==").expect("decodes"), b"foob");
        assert_eq!(decode("Zm9vYmE=").expect("decodes"), b"fooba");
        assert_eq!(decode("Zm9vYmFy").expect("decodes"), b"foobar");
    }

    #[test]
    fn round_trips_every_byte_value() {
        let all: Vec<u8> = (0u8..=255).collect();
        assert_eq!(decode(&encode(&all)).expect("decodes"), all);
    }

    #[test]
    fn round_trips_every_length_up_to_the_padding_cycle() {
        // Lengths 0..=8 cover all three padding cases twice over.
        for len in 0..=8usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 % 256) as u8).collect();
            let encoded = encode(&bytes);
            assert_eq!(
                decode(&encoded).expect("decodes"),
                bytes,
                "round trip failed at length {len} (encoded {encoded:?})"
            );
        }
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        // This decodes untrusted guest input. Every one of these must be an
        // error, never a panic and never a silent wrong answer.
        for bad in [
            "Z",          // length 1 mod 4 is impossible
            "Zm9vY",      // length 5, also 1 mod 4
            "Zg=",        // truncated padding
            "Zg===",      // over-padded
            "Zm$v",       // character outside the alphabet
            "Zm9v=",      // padding in a full quantum
            "=Zm9",       // leading padding
            "Zm=v",       // padding in the middle
            "こんにちは", // multi-byte UTF-8
        ] {
            assert!(
                decode(bad).is_err(),
                "{bad:?} must be rejected, got {:?}",
                decode(bad)
            );
        }
    }

    #[test]
    fn decode_is_total_over_short_arbitrary_strings() {
        // Exhaustive over a small alphabet: no input may panic. This is the
        // property that matters for a decoder facing untrusted input — not that
        // it accepts the right things, but that it never crashes on the wrong
        // ones.
        const ALPHABET: [char; 6] = ['A', 'Z', 'm', '9', '=', '$'];
        let mut checked = 0usize;
        for len in 0..=4usize {
            for mut code in 0..ALPHABET.len().pow(len as u32) {
                let candidate: String = (0..len)
                    .map(|_| {
                        let c = ALPHABET[code % ALPHABET.len()];
                        code /= ALPHABET.len();
                        c
                    })
                    .collect();
                // Must not panic. Either outcome is acceptable.
                let _ = decode(&candidate);
                checked += 1;
            }
        }
        // 6^0 + 6^1 + 6^2 + 6^3 + 6^4 = 1555
        assert_eq!(checked, 1555, "enumeration changed; update deliberately");
    }
}
