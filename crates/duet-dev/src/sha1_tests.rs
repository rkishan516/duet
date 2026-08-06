//! SHA-1 against published vectors.
//!
//! A hand-rolled hash with no test vectors is a hand-rolled bug. These are
//! RFC 3174's own examples plus, crucially, the worked handshake example from
//! RFC 6455 §1.3 — the only computation this implementation is ever actually
//! asked to perform.

use super::sha1;
use crate::base64;

fn hex(digest: [u8; 20]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_rfc_3174_vectors() {
    // The three canonical vectors, plus the empty input, which is the one a
    // padding bug hits first: it exercises the "message is nothing but
    // padding" path.
    let cases: [(&str, &str); 4] = [
        ("", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        ("abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
        (
            "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
        ),
        (
            "The quick brown fox jumps over the lazy dog",
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(
            hex(sha1(input.as_bytes())),
            want,
            "SHA-1 of {input:?} is wrong"
        );
    }
}

#[test]
fn the_block_boundary_lengths_where_padding_goes_wrong() {
    // Padding is the part of SHA-1 that breaks, and it breaks at the lengths
    // where the length field does or does not fit in the final block: 55 bytes
    // fits with one byte to spare, 56 forces a whole extra block, 64 is
    // exactly one block. A million 'a's checks the multi-block loop and the
    // 64-bit length field together.
    let cases: [(usize, &str); 4] = [
        (55, "c1c8bbdc22796e28c0e15163d20899b65621d65a"),
        (56, "c2db330f6083854c99d4b5bfb6e8f29f201be699"),
        (64, "0098ba824b5c16427bd7a1122a5a442a25ec644d"),
        (1_000_000, "34aa973cd4c4daa4f61eeb2bdbad27316534016f"),
    ];
    for (len, want) in cases {
        let input = vec![b'a'; len];
        assert_eq!(hex(sha1(&input)), want, "SHA-1 of {len} 'a' bytes is wrong");
    }
}

#[test]
fn the_rfc_6455_handshake_example() {
    // §1.3's worked example: key "dGhlIHNhbXBsZSBub25jZQ==" plus the GUID must
    // hash and encode to "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=". This is the exact
    // computation `ws::verify_handshake` performs, so if this passes the
    // handshake check is correct end to end even though neither the hash nor
    // the encoder is tested against the other anywhere else.
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let guid = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let accept = base64::encode(&sha1(format!("{key}{guid}").as_bytes()));
    assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}
