//! Base64 against RFC 4648's own vectors.

use super::encode;

#[test]
fn the_rfc_4648_vectors() {
    // §10's table. The point of all seven is the padding: input lengths
    // 0,1,2,3,4,5,6 cycle through every `len % 3` case twice, which is where
    // an encoder's `=` handling goes wrong.
    let cases: [(&str, &str); 7] = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];
    for (input, want) in cases {
        assert_eq!(encode(input.as_bytes()), want, "base64({input:?}) is wrong");
    }
}

#[test]
fn every_alphabet_character_is_reachable_and_correct() {
    // Encodes 0x00..0xFF, which covers all 64 output characters including `+`
    // and `/` — the two a URL-safe alphabet would get wrong, and the two that
    // appear in real `Sec-WebSocket-Accept` values.
    let all: Vec<u8> = (0u8..=255).collect();
    let encoded = encode(&all);
    for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".chars() {
        assert!(
            encoded.contains(c),
            "encoding 0x00..0xff should produce {c:?} somewhere, got {encoded}"
        );
    }
    // 256 bytes is not a multiple of 3, so the output must be padded and a
    // multiple of four long.
    assert_eq!(encoded.len() % 4, 0, "base64 output is always 4-aligned");
    assert!(
        encoded.ends_with('='),
        "256 % 3 == 1, so this must be padded"
    );
}

#[test]
fn the_length_of_the_output_is_always_the_padded_one() {
    // A `Sec-WebSocket-Key` is exactly 16 bytes and its encoding is exactly 24
    // characters; a digest is 20 bytes and encodes to 28. Both are values the
    // handshake puts on the wire, and a server will reject a key of the wrong
    // length outright.
    assert_eq!(encode(&[0u8; 16]).len(), 24);
    assert_eq!(encode(&[0u8; 20]).len(), 28);
}
