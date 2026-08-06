/**
 * Standard base64 (RFC 4648) with padding, for `Value::Bytes`.
 *
 * Hand-written rather than delegating to `atob`/`btoa` or Node's `Buffer`, for
 * two reasons:
 *
 * - **Availability.** This package runs in a `wry` webview, in a browser and
 *   under Node. `Buffer` exists only in the last of those, and `atob` is a
 *   legacy DOM API that Node marks deprecated.
 * - **Strictness.** `atob` accepts input this wire format must refuse: missing
 *   padding, embedded whitespace, and non-canonical trailing bits (`"Zh=="`
 *   and `"Zg=="` both decode to `f` under `atob`, so four distinct strings map
 *   to one byte sequence). `duet_codec::base64` (crates/duet-codec/src/base64.rs)
 *   refuses all of those, and a guest that accepted what the host rejects is a
 *   cross-language divergence of exactly the kind the golden corpus exists to
 *   prevent.
 *
 * This module mirrors that Rust decoder rule for rule.
 *
 * @module
 */

const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

/**
 * Maps a base64 character code to its 6-bit value, or `-1` if it is not in the
 * alphabet.
 *
 * A lookup table rather than `ALPHABET.indexOf`, so decoding stays O(n) rather
 * than O(n·64) on untrusted input.
 */
const SEXTETS: Int8Array = buildSextetTable();

function buildSextetTable(): Int8Array {
  // 128 entries: every character outside ASCII is out of the alphabet anyway,
  // and is caught by the range check in `sextet` below.
  const table = new Int8Array(128).fill(-1);
  for (let i = 0; i < ALPHABET.length; i++) {
    table[ALPHABET.charCodeAt(i)] = i;
  }
  return table;
}

function sextet(code: number): number {
  return code < 128 ? (SEXTETS[code] as number) : -1;
}

/** Encodes `bytes` as standard base64 with padding. */
export function encodeBase64(bytes: Uint8Array): string {
  let out = '';
  for (let i = 0; i < bytes.length; i += 3) {
    const remaining = bytes.length - i;
    const b0 = bytes[i] as number;
    const b1 = remaining > 1 ? (bytes[i + 1] as number) : 0;
    const b2 = remaining > 2 ? (bytes[i + 2] as number) : 0;
    const triple = (b0 << 16) | (b1 << 8) | b2;

    out += ALPHABET[(triple >> 18) & 0x3f];
    out += ALPHABET[(triple >> 12) & 0x3f];
    out += remaining > 1 ? ALPHABET[(triple >> 6) & 0x3f] : '=';
    out += remaining > 2 ? ALPHABET[triple & 0x3f] : '=';
  }
  return out;
}

/**
 * Decodes standard base64 with padding.
 *
 * Returns `null` for any input that is not exactly well-formed: wrong length,
 * characters outside the alphabet, misplaced padding, or a padded quantum whose
 * discarded low bits are not zero. The caller turns that into a
 * `DuetCodecError` carrying `bad_base64` — this module stays free of the error
 * type so it can be read against the Rust original side by side.
 *
 * `null` rather than a thrown error because "not base64" is an expected outcome
 * on a decode path for untrusted input, not an exceptional one.
 */
export function decodeBase64(s: string): Uint8Array | null {
  if (s.length % 4 !== 0) return null;

  const quanta = s.length / 4;
  const out = new Uint8Array(quanta * 3);
  let written = 0;

  for (let q = 0; q < quanta; q++) {
    const at = q * 4;
    const isLast = q === quanta - 1;

    let pad = 0;
    for (let i = 0; i < 4; i++) {
      if (s.charCodeAt(at + i) === 0x3d) pad++;
    }
    // Padding may appear only in the final quantum, at most twice, and only as
    // a suffix: "Zg==" is legal, "Z=g=" is not.
    if (pad > 0 && !isLast) return null;
    if (pad > 2) return null;
    for (let i = 4 - pad; i < 4; i++) {
      if (s.charCodeAt(at + i) !== 0x3d) return null;
    }

    let acc = 0;
    for (let i = 0; i < 4 - pad; i++) {
      const v = sextet(s.charCodeAt(at + i));
      if (v < 0) return null;
      acc = (acc << 6) | v;
    }
    // A padded quantum carries more sextet bits than output bytes: the (4-pad)
    // sextets hold (4-pad)*6 bits for (3-pad) output bytes, i.e. (3-pad)*8 bits
    // — a remainder of 2*pad low bits that canonical base64 requires to be
    // zero. A non-zero remainder means the input encodes more information than
    // the output bytes can hold, which this codec must reject rather than
    // silently drop (four distinct strings would otherwise decode to the same
    // bytes).
    if (pad > 0 && (acc & ((1 << (2 * pad)) - 1)) !== 0) return null;
    acc <<= 6 * pad;

    out[written++] = (acc >> 16) & 0xff;
    if (pad < 2) out[written++] = (acc >> 8) & 0xff;
    if (pad < 1) out[written++] = acc & 0xff;
  }

  return out.subarray(0, written);
}
