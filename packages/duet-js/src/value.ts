/**
 * The Duet value tree and its tagged-JSON codec.
 *
 * Mirrors `duet_core::Value` (crates/duet-core/src/value.rs), `duet-codec`'s
 * encoding of it (crates/duet-codec/src/value.rs), and the Dart port in
 * `packages/duet/lib/src/duet_value.dart`.
 *
 * # Why a plain discriminated union and free functions
 *
 * Dart uses a `sealed` class hierarchy with `toJson` methods; the natural
 * TypeScript equivalent is a discriminated union of plain objects. That is not
 * only idiomatic — it is what lets a decoded value survive `structuredClone`
 * and a `postMessage` hop between a worker and a page with nothing lost, and it
 * keeps values cheap to build and compare. The codec is therefore a set of free
 * functions rather than methods.
 *
 * @module
 */

import { decodeBase64, encodeBase64 } from './base64.ts';
import { DuetCodecError, DuetReason, echoBounded, MAX_JSON_DEPTH } from './errors.ts';
import {
  asJsonObject,
  decodeDuetJson,
  encodeDuetJson,
  hasOwnField,
  isCanonicalSignedDecimal,
  jsonTypeName,
  type CanonicalJson,
  type JsonObject,
} from './json.ts';

/** The absence of a value, distinct from a path that does not exist at all. */
export interface DuetNull {
  readonly kind: 'null';
}

/** A boolean. */
export interface DuetBool {
  readonly kind: 'bool';
  readonly value: boolean;
}

/**
 * A signed 64-bit integer.
 *
 * `bigint`, not `number`, and this is not negotiable: the wire's integer domain
 * is `i64`, and the golden corpus pins `9007199254740993` — one above 2^53,
 * where a JavaScript `number` starts skipping odd integers. A `number`-backed
 * client reads that value as `9007199254740992` and re-emits it wrong, with no
 * error anywhere. The same argument is why the wire carries integers as decimal
 * *strings* in the first place.
 */
export interface DuetInt {
  readonly kind: 'int';
  readonly value: bigint;
}

/** A 64-bit float, including NaN and both infinities. */
export interface DuetFloat {
  readonly kind: 'float';
  readonly value: number;
}

/** A string. */
export interface DuetStr {
  readonly kind: 'str';
  readonly value: string;
}

/**
 * Raw bytes.
 *
 * Kept distinguishable from {@link DuetStr} by its own tag even though both
 * encode to a JSON string — the single clearest reason the encoding is tagged
 * at all.
 */
export interface DuetBytes {
  readonly kind: 'bytes';
  readonly value: Uint8Array;
}

/** An ordered list of values. */
export interface DuetList {
  readonly kind: 'list';
  readonly items: readonly DuetValue[];
}

/**
 * A string-keyed map of values.
 *
 * A `Map`, never a plain object. Two reasons, both load-bearing: a `Map`
 * preserves insertion order for integer-like keys where a plain object does not
 * (see {@link encodeDuetJson}), and a `Map` key named `__proto__`,
 * `constructor` or `toString` is just a key, where on a plain object it
 * collides with the prototype chain.
 */
export interface DuetMap {
  readonly kind: 'map';
  readonly entries: ReadonlyMap<string, DuetValue>;
}

/** One value in the host's shared state tree. */
export type DuetValue =
  | DuetNull
  | DuetBool
  | DuetInt
  | DuetFloat
  | DuetStr
  | DuetBytes
  | DuetList
  | DuetMap;

/** The inclusive bottom of the `i64` domain a {@link DuetInt} may hold. */
export const MIN_DUET_INT = -9223372036854775808n;

/** The inclusive top of the `i64` domain a {@link DuetInt} may hold. */
export const MAX_DUET_INT = 9223372036854775807n;

/**
 * The most characters an `"i"` payload may have: 19 digits for `i64::MAX`, plus
 * a sign.
 *
 * Checked before `BigInt()` is called, not after. `BigInt` on a million-digit
 * string is super-linear work and a multi-megabyte allocation, both driven by
 * whatever the peer sent — a denial-of-service shape on a hot IPC path. No
 * string longer than this can name a value in the domain, so refusing it up
 * front costs nothing and closes the hole.
 */
const MAX_INT_PAYLOAD_CHARS = 20;

/** The one null value. */
export function duetNull(): DuetNull {
  return { kind: 'null' };
}

/** Wraps a boolean. */
export function duetBool(value: boolean): DuetBool {
  return { kind: 'bool', value };
}

/**
 * Wraps a signed 64-bit integer.
 *
 * @throws DuetCodecError with `bad_int` if `value` is outside
 *   {@link MIN_DUET_INT}..{@link MAX_DUET_INT}. `bigint` is unbounded, so
 *   without this check a caller could build a value that every peer refuses and
 *   only find out at the far end of an IPC round trip.
 */
export function duetInt(value: bigint): DuetInt {
  if (value < MIN_DUET_INT || value > MAX_DUET_INT) {
    throw new DuetCodecError(
      DuetReason.badInt,
      `${value.toString()} is outside the wire's i64 integer domain`,
    );
  }
  return { kind: 'int', value };
}

/** Wraps a double, which may be NaN or either infinity. */
export function duetFloat(value: number): DuetFloat {
  return { kind: 'float', value };
}

/** Wraps a string. */
export function duetStr(value: string): DuetStr {
  return { kind: 'str', value };
}

/** Wraps raw bytes. The array is not copied; treat it as owned by this value. */
export function duetBytes(value: Uint8Array): DuetBytes {
  return { kind: 'bytes', value };
}

/** Wraps an ordered list of values. */
export function duetList(items: readonly DuetValue[]): DuetList {
  return { kind: 'list', items };
}

/**
 * Wraps map entries, in whatever order the caller supplies them —
 * {@link encodeValue} sorts on the way out, so the order here never reaches the
 * wire.
 *
 * Accepts a plain object for convenience, but note that going through one
 * reorders integer-like keys before this function ever sees them. That is
 * harmless (the encoder sorts anyway) and is called out only so nobody
 * concludes from it that plain objects are safe on the *encode* path — see
 * {@link encodeDuetJson}.
 */
export function duetMap(
  entries: ReadonlyMap<string, DuetValue> | Iterable<readonly [string, DuetValue]> | Record<string, DuetValue>,
): DuetMap {
  if (entries instanceof Map) return { kind: 'map', entries };
  if (typeof (entries as Iterable<readonly [string, DuetValue]>)[Symbol.iterator] === 'function') {
    return { kind: 'map', entries: new Map(entries as Iterable<readonly [string, DuetValue]>) };
  }
  return { kind: 'map', entries: new Map(Object.entries(entries as Record<string, DuetValue>)) };
}

/**
 * Encodes a value to its tagged JSON form, ready for {@link encodeDuetJson}.
 *
 * @param value the value to encode
 * @returns the tagged document
 */
export function encodeValue(value: DuetValue): CanonicalJson {
  switch (value.kind) {
    case 'null':
      return new Map<string, CanonicalJson>([['t', 'n']]);
    case 'bool':
      return tagged('bool', value.value);
    case 'int':
      // A decimal **string**, never a JSON number
      // (crates/duet-codec/src/value.rs's `encode_value`). A JSON number above
      // 2^53 loses precision the moment any JavaScript reader parses it.
      // `BigInt.prototype.toString` is already canonical — no `+`, no leading
      // zeros, no `-0` — which is what `is_canonical_signed_decimal` requires.
      return tagged('i', value.value.toString());
    case 'float':
      return tagged('f', encodeFloat(value.value));
    case 'str':
      return tagged('s', value.value);
    case 'bytes':
      return tagged('b', encodeBase64(value.value));
    case 'list':
      return tagged('l', value.items.map(encodeValue));
    case 'map':
      return tagged(
        'm',
        new Map<string, CanonicalJson>(
          [...value.entries].map(([key, item]) => [key, encodeValue(item)]),
        ),
      );
  }
}

/** Encodes a value as the exact text the wire carries. */
export function encodeValueText(value: DuetValue): string {
  return encodeDuetJson(encodeValue(value));
}

/**
 * Decodes one tagged value from wire text.
 *
 * Throws {@link DuetCodecError} and nothing else, whatever `text` contains —
 * including text that is not JSON, and text nested past {@link MAX_JSON_DEPTH}.
 */
export function decodeValueText(text: string): DuetValue {
  return decodeValue(decodeDuetJson(text));
}

/**
 * Decodes one tagged value from already-parsed JSON.
 *
 * Total over all JSON input: every branch either returns a {@link DuetValue} or
 * throws {@link DuetCodecError} — never a bare `TypeError` from an unchecked
 * property access, which would surface as an unrelated failure instead of a
 * diagnosable message naming the field and the tag.
 */
export function decodeValue(json: unknown): DuetValue {
  return decodeValueAt(json, 1);
}

/**
 * Decodes a value that the wire allows to be absent.
 *
 * JSON `null` means "no value at this path" — distinct from `{"t":"n"}`, which
 * is a {@link DuetNull}. The wire draws the same distinction; a client that
 * collapsed the two would lose it. See `optional_value` in
 * crates/duet-protocol/src/wire.rs.
 */
export function decodeOptionalValue(json: unknown): DuetValue | null {
  return json === null ? null : decodeValue(json);
}

/** Encodes a value that the wire allows to be absent. */
export function encodeOptionalValue(value: DuetValue | null): CanonicalJson {
  return value === null ? null : encodeValue(value);
}

/**
 * Structural equality, mirroring Rust's derived `PartialEq` on
 * `duet_core::Value`.
 *
 * That means IEEE-754 semantics leak through for floats: a `NaN` value equals
 * nothing, not even itself, and `-0.0` equals `0.0` despite encoding
 * differently. This is why the golden corpus compares floats by their **bits**
 * rather than through this function — a conformance test written on `==`
 * semantics would be vacuous for every `NaN` case and blind to a lost sign bit.
 */
export function duetValueEquals(a: DuetValue, b: DuetValue): boolean {
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case 'null':
      return true;
    case 'bool':
      return a.value === (b as DuetBool).value;
    case 'int':
      return a.value === (b as DuetInt).value;
    case 'float':
      return a.value === (b as DuetFloat).value;
    case 'str':
      return a.value === (b as DuetStr).value;
    case 'bytes': {
      const other = (b as DuetBytes).value;
      if (a.value.length !== other.length) return false;
      for (let i = 0; i < a.value.length; i++) {
        if (a.value[i] !== other[i]) return false;
      }
      return true;
    }
    case 'list': {
      const other = (b as DuetList).items;
      if (a.items.length !== other.length) return false;
      return a.items.every((item, i) => duetValueEquals(item, other[i] as DuetValue));
    }
    case 'map': {
      const other = (b as DuetMap).entries;
      if (a.entries.size !== other.size) return false;
      for (const [key, item] of a.entries) {
        const mine = other.get(key);
        if (mine === undefined || !duetValueEquals(item, mine)) return false;
      }
      return true;
    }
  }
}

/** Builds the two-field tagged object every non-null value encodes to. */
function tagged(tag: string, payload: CanonicalJson): CanonicalJson {
  return new Map<string, CanonicalJson>([
    ['t', tag],
    ['v', payload],
  ]);
}

/**
 * The four doubles with no portable JSON-number spelling travel as string
 * sentinels (crates/duet-codec/src/value.rs's `encode_float`).
 *
 * `NaN` and the infinities have no JSON literal at all. `-0` is different: JSON
 * *has* a spelling for it and Rust emits one happily, but `JSON.stringify(-0)`
 * is `"0"`, so no JavaScript guest can produce it without a hand-rolled
 * serializer. The sentinel exists for exactly that reason, and all three
 * implementations emit it — the wire must have one spelling per value, not one
 * per language.
 *
 * `Object.is(n, -0)` is the test, not `n === -0`: `-0 === 0` is true in
 * JavaScript exactly as in IEEE 754, so an equality check would tag every zero
 * as negative.
 */
function encodeFloat(n: number): CanonicalJson {
  if (Number.isNaN(n)) return 'NaN';
  if (n === Infinity) return 'Infinity';
  if (n === -Infinity) return '-Infinity';
  if (Object.is(n, -0)) return '-0';
  return n;
}

/**
 * The recursion, carrying the number of JSON containers enclosing `json`
 * (counting the tagged object itself).
 *
 * {@link decodeDuetJson} already refuses over-deep wire text, so this bound
 * fires only when a caller hands an over-deep tree straight to
 * {@link decodeValue}. It is kept anyway: an unbounded recursive decoder is a
 * stack overflow waiting for the first caller who forgets, and in JavaScript
 * that surfaces as a `RangeError` from outside this package's error type.
 */
function decodeValueAt(json: unknown, depth: number): DuetValue {
  if (depth > MAX_JSON_DEPTH) {
    throw new DuetCodecError(
      DuetReason.badJson,
      `value nests deeper than ${MAX_JSON_DEPTH} JSON containers`,
    );
  }
  const obj = asJsonObject(json, 'a value');
  const tag = readTag(obj);

  if (tag === 'n') return { kind: 'null' };

  // Each arm reads "v" for itself, and only once the arm for that specific tag
  // has matched, so an unrecognised tag is refused without the decoder ever
  // going looking for a payload that would not have helped. Mirrors
  // duet-codec's `decode_value`.
  const payload = (): unknown => {
    if (!hasOwnField(obj, 'v')) {
      throw new DuetCodecError(DuetReason.badShape, `tag "${tag}" requires "v"`);
    }
    return obj['v'];
  };

  switch (tag) {
    case 'bool': {
      const v = payload();
      if (typeof v !== 'boolean') {
        throw new DuetCodecError(DuetReason.badShape, '"bool" payload must be a boolean');
      }
      return { kind: 'bool', value: v };
    }
    case 'i':
      return { kind: 'int', value: decodeInt(payload()) };
    case 'f':
      return { kind: 'float', value: decodeFloat(payload()) };
    case 's': {
      const v = payload();
      if (typeof v !== 'string') {
        throw new DuetCodecError(DuetReason.badShape, '"s" payload must be a string');
      }
      return { kind: 'str', value: v };
    }
    case 'b':
      return { kind: 'bytes', value: decodeBytes(payload()) };
    case 'l': {
      const v = payload();
      if (!Array.isArray(v)) {
        throw new DuetCodecError(DuetReason.badShape, '"l" payload must be an array');
      }
      // depth + 2, not + 1: each nested value sits inside both the `[...]`
      // array and its own `{"t":...}` object.
      return { kind: 'list', items: v.map((item) => decodeValueAt(item, depth + 2)) };
    }
    case 'm': {
      const v = payload();
      if (v === null || typeof v !== 'object' || Array.isArray(v)) {
        throw new DuetCodecError(DuetReason.badShape, '"m" payload must be an object');
      }
      const source = v as JsonObject;
      const entries = new Map<string, DuetValue>();
      for (const key of Object.keys(source)) {
        entries.set(key, decodeValueAt(source[key], depth + 2));
      }
      return { kind: 'map', entries };
    }
    default:
      throw new DuetCodecError(DuetReason.unknownTag, `unknown type tag ${echoBounded(tag)}`);
  }
}

/** Reads the `"t"` discriminator, refusing a missing or non-string one. */
function readTag(obj: JsonObject): string {
  if (!hasOwnField(obj, 't')) {
    throw new DuetCodecError(DuetReason.badShape, 'missing "t"');
  }
  const tag = obj['t'];
  if (typeof tag !== 'string') {
    throw new DuetCodecError(DuetReason.badShape, '"t" must be a string');
  }
  return tag;
}

/** Decodes an `"i"` payload: a canonical decimal string that fits in an `i64`. */
function decodeInt(payload: unknown): bigint {
  if (typeof payload !== 'string') {
    throw new DuetCodecError(DuetReason.badInt, '"i" payload must be a decimal string');
  }
  if (payload.length > MAX_INT_PAYLOAD_CHARS) {
    throw new DuetCodecError(
      DuetReason.badInt,
      `"i" payload is too long to name an i64: ${echoBounded(payload)}`,
    );
  }
  if (!isCanonicalSignedDecimal(payload)) {
    throw new DuetCodecError(
      DuetReason.badInt,
      `"i" payload is not canonical: ${echoBounded(payload)}`,
    );
  }
  const parsed = BigInt(payload);
  if (parsed < MIN_DUET_INT || parsed > MAX_DUET_INT) {
    throw new DuetCodecError(
      DuetReason.badInt,
      `"i" payload overflows i64: ${echoBounded(payload)}`,
    );
  }
  return parsed;
}

/**
 * Decodes an `"f"` payload: either a JSON number, or one of the four sentinel
 * strings for the doubles with no portable JSON-number spelling.
 *
 * Deliberately wider than {@link encodeFloat}, mirroring `duet_codec`'s
 * `decode_float`: any JSON number is accepted, so `1` decodes to `1.0` and a
 * literal `-0` still decodes with its sign. A guest hand-building a value has
 * no way to force a decimal point — `JSON.stringify(1.0)` is `"1"` — and should
 * not have to know which spelling this library happens to emit.
 */
function decodeFloat(payload: unknown): number {
  if (typeof payload === 'number') return payload;
  if (typeof payload === 'string') {
    switch (payload) {
      case 'NaN':
        return NaN;
      case 'Infinity':
        return Infinity;
      case '-Infinity':
        return -Infinity;
      case '-0':
        return -0;
      default:
        throw new DuetCodecError(
          DuetReason.badFloat,
          `unrecognised float sentinel ${echoBounded(payload)}`,
        );
    }
  }
  throw new DuetCodecError(
    DuetReason.badFloat,
    `"f" payload must be a number or a sentinel string, found ${jsonTypeName(payload)}`,
  );
}

/** Decodes a `"b"` payload, turning "not base64" into this package's own error. */
function decodeBytes(payload: unknown): Uint8Array {
  if (typeof payload !== 'string') {
    throw new DuetCodecError(DuetReason.badBase64, '"b" payload must be a string');
  }
  const bytes = decodeBase64(payload);
  if (bytes === null) {
    throw new DuetCodecError(DuetReason.badBase64, `invalid base64: ${echoBounded(payload)}`);
  }
  return bytes;
}
