/**
 * The seam between a TypeScript type and the host's {@link DuetValue} tree.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_codec.dart`.
 *
 * @module
 */

import {
  duetBool,
  duetBytes,
  duetFloat,
  duetInt,
  duetList,
  duetMap,
  duetStr,
  type DuetValue,
} from '../value.ts';

/**
 * Encodes and decodes one TypeScript type to and from a {@link DuetValue}.
 *
 * Generated code implements this once per schema type; the primitive codecs
 * below cover the leaves.
 *
 * # The non-nullable bound is load-bearing, not stylistic
 *
 * {@link DuetCodec.decode} answers `null` for "this value is not a `T`". That
 * spelling is only unambiguous because `T` itself can never *be* null: with a
 * nullable type argument, `DuetCodec<string | null>.decode` returning `null`
 * would mean either "refused" or "decoded, and the answer is null", and nothing
 * at the call site could tell the two apart.
 *
 * That ambiguity is not cosmetic here. The whole client is built on the
 * distinction between a path that holds a value of kind `'null'` and a path that
 * does not exist — {@link DuetClient.get} returns `null` for the second and a
 * `'null'` value for the first, and the wire spends a distinct spelling on each
 * ({@link decodeOptionalValue}). A codec that could not tell "refused" from
 * "null" would collapse exactly that distinction at the one layer whose job is
 * to preserve it.
 *
 * `T extends {}` — TypeScript's spelling for "anything but `null` and
 * `undefined`" — rejects a nullable type argument **at the definition site**, so
 * no such codec can be written at all. An optional field is expressed by pairing
 * a non-nullable codec with {@link DuetOptionalField}, which keeps "absent",
 * "null" and "a `T`" as three separate outcomes.
 */
export interface DuetCodec<T extends {}> {
  /**
   * The name of the type this codec decodes, for error messages.
   *
   * Used to build the `reason` on a {@link DuetMismatch}, which is the only
   * account an application gets of a type disagreement between two guests
   * sharing one store. `'string'`, `'Editor'`, `'bigint[]'`.
   */
  readonly name: string;

  /**
   * Encodes `value` for the wire. Must be total: an encoder that can throw turns
   * a typed `set` into a crash on data the application already holds.
   */
  encode(value: T): DuetValue;

  /**
   * Decodes `value`, or answers `null` if it is not a `T`.
   *
   * Should be total. A throw is *tolerated* — {@link duetRequiredReading} and
   * {@link duetOptionalReading} catch it and report a mismatch — but it costs
   * the caller the diagnosis a `null` plus {@link DuetCodec.name} would have
   * given.
   */
  decode(value: DuetValue): T | null;
}

/** A codec for `boolean`, mirroring Rust's `bool` and `Value::Bool`. */
export const duetBoolCodec: DuetCodec<boolean> = {
  name: 'boolean',
  encode: (value) => duetBool(value),
  decode: (value) => (value.kind === 'bool' ? value.value : null),
};

/**
 * A codec for `bigint`, mirroring Rust's `i64` and `Value::Int`.
 *
 * `bigint`, not `number`, for the same reason {@link DuetInt} is: the wire's
 * integer domain is `i64`, and a `number`-backed codec reads `9007199254740993`
 * as `9007199254740992` and re-emits it wrong with no error anywhere.
 */
export const duetIntCodec: DuetCodec<bigint> = {
  name: 'bigint',
  encode: (value) => duetInt(value),
  decode: (value) => (value.kind === 'int' ? value.value : null),
};

/**
 * A codec for `number`, mirroring Rust's `f64` and `Value::Float`.
 *
 * Accepts only a `'float'`. An integer on the wire is **not** widened: `i64` and
 * `f64` are distinct host types, and a codec that quietly accepted one for the
 * other would let a guest write an `i64` where the schema says `f64` and have
 * every other guest agree — the exact cross-guest type drift
 * {@link DuetMismatch} exists to surface.
 */
export const duetFloatCodec: DuetCodec<number> = {
  name: 'number',
  encode: (value) => duetFloat(value),
  decode: (value) => (value.kind === 'float' ? value.value : null),
};

/** A codec for `string`, mirroring Rust's `String` and `Value::Str`. */
export const duetStringCodec: DuetCodec<string> = {
  name: 'string',
  encode: (value) => duetStr(value),
  decode: (value) => (value.kind === 'str' ? value.value : null),
};

/**
 * A codec for raw bytes, mirroring Rust's `Vec<u8>` and `Value::Bytes`.
 *
 * Distinct from {@link duetStringCodec} even though both travel as a JSON
 * string: the wire tags them apart, and so does this.
 */
export const duetBytesCodec: DuetCodec<Uint8Array> = {
  name: 'Uint8Array',
  encode: (value) => duetBytes(value),
  decode: (value) => (value.kind === 'bytes' ? value.value : null),
};

/**
 * A codec for a homogeneous list, mirroring Rust's `Vec<T>` and `Value::List`.
 *
 * Refuses the whole list if any element is refused, rather than dropping the bad
 * element: a partial list is a wrong answer that looks like a right one.
 */
export function duetListCodec<T extends {}>(item: DuetCodec<T>): DuetCodec<T[]> {
  return {
    name: `${item.name}[]`,
    encode: (value) => duetList(value.map((element) => item.encode(element))),
    decode: (value) => {
      if (value.kind !== 'list') return null;
      const out: T[] = [];
      for (const element of value.items) {
        const decoded = item.decode(element);
        if (decoded === null) return null;
        out.push(decoded);
      }
      return out;
    },
  };
}

/**
 * A codec for a string-keyed map, mirroring Rust's `BTreeMap<String, T>` and
 * `Value::Map`.
 *
 * A `Map`, never a plain object, for the reasons {@link DuetMap} gives: integer-
 * like key order, and `__proto__` being a key rather than a prototype.
 */
export function duetMapCodec<T extends {}>(
  value: DuetCodec<T>,
): DuetCodec<Map<string, T>> {
  return {
    name: `Map<string, ${value.name}>`,
    encode: (map) =>
      duetMap(new Map([...map].map(([key, item]) => [key, value.encode(item)]))),
    decode: (found) => {
      if (found.kind !== 'map') return null;
      const out = new Map<string, T>();
      for (const [key, item] of found.entries) {
        const decoded = value.decode(item);
        if (decoded === null) return null;
        out.set(key, decoded);
      }
      return out;
    },
  };
}
