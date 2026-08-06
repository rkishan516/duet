//! Base64 encoding, for exactly two values in the WebSocket handshake: the
//! 16-byte `Sec-WebSocket-Key` this client sends, and the 20-byte digest it
//! compares against the server's `Sec-WebSocket-Accept`.
//!
//! Encode only. There is no decoder here because nothing in this crate ever
//! decodes base64 — the accept check compares *encoded* strings, which is both
//! simpler and exactly what RFC 6455 §4.1 specifies.
//!
//! `duet-codec` has its own base64 (`crates/duet-codec/src/base64.rs`), and it
//! is `pub(crate)` there. Making it public to share ~25 lines would widen a
//! published crate's API surface permanently for a private need of a dev-only
//! one; that crate's encoder is part of how `Bytes` values reach a guest, and
//! coupling the WebSocket handshake to it would mean a future change to the
//! wire format has to consider this file too. Two small independent copies is
//! the cheaper coupling here, and both are directly tested against RFC 4648's
//! vectors.

/// The standard alphabet (RFC 4648 §4), with `=` padding.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `bytes` as standard, padded base64.
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Pad the group to three bytes; `chunk.len()` then says how many of the
        // four output characters are real rather than `=`.
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let group = (b0 << 16) | (b1 << 8) | b2;

        // Every index is masked to 6 bits, so it is always in 0..64 and the
        // lookup cannot be out of bounds.
        out.push(ALPHABET[((group >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((group >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((group >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(group & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
#[path = "base64_tests.rs"]
mod tests;
