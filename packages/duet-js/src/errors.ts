/**
 * Every error this package throws, and the machine-readable reason codes they
 * carry.
 *
 * One class hierarchy rooted at {@link DuetError}, so a caller can write
 * `catch (e) { if (e instanceof DuetError) ... }` and be sure nothing else
 * escapes — and so a test can assert that a malformed message produced *this
 * package's* refusal rather than a `SyntaxError` from `JSON.parse`, a
 * `TypeError` from an unchecked property access or a `RangeError` from a
 * blown stack leaking out of a lower layer.
 *
 * Mirrors `packages/duet/lib/src/duet_error.dart` and
 * `duet_codec::CodecError` (crates/duet-codec/src/error.rs).
 *
 * @module
 */

/**
 * The stable, machine-readable names for *why* a wire payload was refused.
 *
 * These mirror `duet_codec::CodecError::reason_code` one for one. They are
 * part of the cross-language contract, not prose: the golden wire corpus at
 * `corpus/wire-corpus.json` records one per reject case, so the Rust, Dart and
 * TypeScript decoders must agree not merely that a message is bad but on
 * *which rule* it broke. A decoder that rejects everything for one blanket
 * reason passes a "does it throw?" test completely while agreeing with nobody.
 *
 * A frozen object rather than a TypeScript `enum`: an `enum` emits runtime
 * code, so it cannot be erased by Node's built-in type stripping, and this
 * package is written to run from source under `node --test` with no build
 * step. The companion type alias below gives the same call-site ergonomics.
 */
export const DuetReason = Object.freeze({
  /** The text is not JSON at all, or its nesting exceeds {@link MAX_JSON_DEPTH}. */
  badJson: 'bad_json',
  /** A `"t"` type tag or a `"kind"` envelope discriminator outside the known set. */
  unknownTag: 'unknown_tag',
  /** Well-formed JSON in the wrong shape: a missing field, or a field whose JSON type is wrong. */
  badShape: 'bad_shape',
  /** An integer payload that is not a canonical decimal string, or is outside its field's domain. */
  badInt: 'bad_int',
  /** A float payload that is neither a JSON number nor one of the four recognised sentinels. */
  badFloat: 'bad_float',
  /** A `Bytes` payload that is not valid base64. */
  badBase64: 'bad_base64',
  /** A path string that does not match the path grammar. */
  badPath: 'bad_path',
} as const);

/** One of {@link DuetReason}'s codes. */
export type DuetReason = (typeof DuetReason)[keyof typeof DuetReason];

/**
 * Every reason code, for tests that assert the set is fully covered.
 *
 * `Object.values` on the frozen object above rather than a second hand-written
 * list, so a reason added there cannot be forgotten here.
 */
export const DUET_REASONS: readonly DuetReason[] = Object.freeze(
  Object.values(DuetReason),
) as readonly DuetReason[];

/**
 * The base of every error this package throws.
 *
 * `abstract`, so the only things that can be one of these are the three
 * subclasses below — which is what makes `e instanceof DuetError` a complete
 * statement about this package's failure modes rather than an open set.
 */
export abstract class DuetError extends Error {}

/**
 * Malformed data on the wire: the bytes could not be turned into a value, a
 * request, a response or a push.
 *
 * Thrown rather than returned in a result object because every decode entry
 * point in this package is total — it either produces a value or throws this —
 * and a result type would let a caller ignore the failure by forgetting to
 * check a field.
 */
export class DuetCodecError extends DuetError {
  /** One of {@link DuetReason}'s codes: which rule the payload broke. */
  readonly reason: DuetReason;

  /**
   * @param reason which rule the payload broke
   * @param message a developer-facing explanation, with any guest- or
   *   host-supplied text already bounded by {@link echoBounded}
   */
  constructor(reason: DuetReason, message: string) {
    super(message);
    this.name = 'DuetCodecError';
    this.reason = reason;
  }
}

/**
 * The exchange failed below or beside the protocol: no reply arrived, or a
 * well-formed response answered a different request than the one sent.
 *
 * Distinct from {@link DuetCodecError}, which means the bytes themselves were
 * malformed, and from {@link DuetFailure}, which means the host understood the
 * request perfectly and refused it.
 */
export class DuetTransportError extends DuetError {
  /** @param message a developer-facing explanation. */
  constructor(message: string) {
    super(message);
    this.name = 'DuetTransportError';
  }
}

/**
 * The host answered `{"kind":"failed", ...}` — a well-formed rejection of a
 * well-formed request, for example a `set` at an unresolvable path.
 *
 * The request reached the host and was understood; only the operation failed.
 */
export class DuetFailure extends DuetError {
  /** The request this failure answers. */
  readonly requestId: bigint;

  /**
   * @param requestId the request this failure answers
   * @param message the host's explanation, safe to show a developer
   */
  constructor(requestId: bigint, message: string) {
    super(message);
    this.name = 'DuetFailure';
    this.requestId = requestId;
  }
}

/**
 * The most characters of guest- or host-supplied text any error message may
 * embed, mirroring `duet_codec::error::MAX_ECHO_CHARS`.
 *
 * Without a bound, a one-megabyte payload becomes a one-megabyte log line on a
 * hot IPC path — a denial-of-service shape reachable by anything that can send
 * this client a message.
 */
export const MAX_ECHO_CHARS = 48;

/**
 * Renders `text` quoted and truncated to {@link MAX_ECHO_CHARS}, for embedding
 * in an error message.
 *
 * Truncation is by UTF-16 code unit, not by code point, so that the cost stays
 * O(1) on the error path — splitting into code points would allocate an array
 * proportional to whatever the peer sent, which is the very thing this bound
 * exists to prevent. A cut that lands between a surrogate pair would leave a
 * lone high surrogate, which renders as a replacement character in most
 * terminals, so the dangling half is dropped.
 */
export function echoBounded(text: string): string {
  if (text.length <= MAX_ECHO_CHARS) return JSON.stringify(text);
  let cut = text.slice(0, MAX_ECHO_CHARS);
  const last = cut.charCodeAt(cut.length - 1);
  if (last >= 0xd800 && last <= 0xdbff) cut = cut.slice(0, -1);
  return `${JSON.stringify(cut).slice(0, -1)}…"`;
}

/**
 * The most nested JSON containers a Duet message may contain: **127**.
 *
 * Mirrors `duet_codec::MAX_JSON_DEPTH` (crates/duet-codec/src/depth.rs), which
 * the host enforces itself over raw text before parsing it. The golden corpus
 * pins the boundary in every language at once: `value/nesting/at_limit` has
 * exactly 127 containers and must decode, `value/nesting/over_limit` has 128 and
 * must be refused as {@link DuetReason.badJson}.
 *
 * # This number must equal the host's exactly, not approximately
 *
 * This package shipped with `128` here against a host that stops at 127, so a
 * document with exactly 128 containers was accepted by TypeScript and refused by
 * Rust. Nothing caught it: the only nesting test in the suite used 200 levels,
 * which every implementation refuses whatever its off-by-one. A limit is only
 * tested by a case on each side of it.
 *
 * JavaScript needs it stated explicitly at all because `JSON.parse` has no
 * limit: V8 accepts a 100 000-deep document without complaint (measured on Node
 * v24). Without this guard a JavaScript guest would accept messages every Rust
 * peer rejects, and — worse — would then hand that tree to this package's
 * recursive decoders, which would overflow the stack for real.
 */
export const MAX_JSON_DEPTH = 127;
