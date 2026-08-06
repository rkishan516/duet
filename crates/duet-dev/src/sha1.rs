//! SHA-1, for exactly one purpose: checking the WebSocket handshake's
//! `Sec-WebSocket-Accept` header (RFC 6455 §4.1).
//!
//! # Why this is here and not a dependency
//!
//! RFC 6455 hard-codes SHA-1 into the opening handshake. It is not being used
//! as a security primitive here and must not be read as one — SHA-1 is
//! collision-broken and unfit for anything that matters. Its job in the
//! handshake is to prove the peer is a WebSocket server rather than an HTTP
//! server or a cache that echoed our request back, which a broken hash still
//! does perfectly well.
//!
//! Given that, taking a crypto crate — and pulling its tree into `duet-cli`,
//! the framework's front door — to compute a hash whose weakness is
//! irrelevant is a poor trade. This is 60 lines of arithmetic checked against
//! the RFC 3174 test vectors plus the RFC 6455 handshake example, which is the
//! only vector that actually matters here.
//!
//! **Do not reuse this for anything else.** It is `pub(crate)`, it has no
//! streaming API, and if a second caller ever appears the right move is to
//! take a real crate rather than to grow this one.

/// The SHA-1 digest of `bytes`, as 20 bytes.
pub(crate) fn sha1(bytes: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    // Padding: the message, a 0x80 byte, zeroes to 56 mod 64, then the length
    // in bits as a big-endian u64.
    let mut message = bytes.to_vec();
    let bit_len = (bytes.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks_exact(4).enumerate() {
            // `chunks_exact(4)` yields exactly 4 bytes, so this cannot fail;
            // the fallback keeps the function total rather than unwrapping.
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
#[path = "sha1_tests.rs"]
mod tests;
