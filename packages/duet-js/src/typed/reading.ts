/**
 * What a typed field currently holds — including the two answers that are not a
 * value.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_reading.dart`.
 *
 * @module
 */

import { echoBounded } from '../errors.ts';
import type { DuetValue } from '../value.ts';
import type { DuetCodec } from './codec.ts';

/** The path holds a value the codec accepted. */
export interface DuetPresent<T extends {}> {
  readonly kind: 'present';
  /** The decoded value. */
  readonly value: T;
}

/**
 * The path exists and holds a value of kind `'null'` — Rust's `Option::None`.
 *
 * Only {@link DuetOptionalField} produces this. A {@link DuetField} whose schema
 * promises a `T` reports a {@link DuetMismatch} for a null, because a null is
 * not the `T` it was promised.
 */
export interface DuetNone {
  readonly kind: 'none';
}

/**
 * There is no node at the path at all.
 *
 * Distinct from {@link DuetNone}: nothing was ever written here, or an ancestor
 * of this path is a scalar, or a list index is out of range. The host says this
 * with a `null` where a value would go; {@link DuetClient.get} says it with
 * `null`.
 */
export interface DuetAbsent {
  readonly kind: 'absent';
}

/**
 * The path holds a value, and the codec refused it.
 *
 * The honest report of two guests disagreeing about one path's type. Carries
 * what was actually found so an application can log it, show it, or write the
 * right type back over it.
 */
export interface DuetMismatch {
  readonly kind: 'mismatch';
  /** The value the host actually holds at the path. */
  readonly found: DuetValue;
  /**
   * Why the codec refused it, already bounded by {@link MAX_ECHO_CHARS} wherever
   * it embeds host-supplied text.
   */
  readonly reason: string;
}

/**
 * One typed observation of one path.
 *
 * # Why a tagged result and not an exception
 *
 * A type mismatch is a **state the host can be in**, not a failure of the call
 * that found it. Another guest can write any value to any path — the two-guest
 * proof has a webview and a Flutter engine writing one store at the same time —
 * so a typed watcher will eventually meet a value its codec refuses. That is
 * data, and it arrives through a push, where there is no call stack to throw
 * into and where throwing would take out the transport's message handler.
 *
 * Given that `watch` cannot throw, `get` **must not** throw either. Two
 * mechanisms for one condition would mean an application that handles a mismatch
 * on its watcher still crashes the first time it reads the same path directly.
 * One tagged type used by `get`, by `watch` and by the router's own mirror is
 * the smaller API: a reader learns it once, and a `switch` over `kind` narrows
 * exhaustively.
 *
 * # Why four arms
 *
 * Because the host can be in exactly four states, and the wire already spends a
 * distinct spelling on each: a decodable value, `{"t":"n"}`, a `null` where a
 * value would go, and a value of some other type. Collapsing any two would
 * delete a distinction the protocol pays for — the same argument
 * {@link decodeOptionalValue} makes one layer down.
 *
 * # Where this diverges from the Dart mirror, and why
 *
 * Dart's version is a `sealed` class hierarchy, so every arm must carry `T` for
 * the hierarchy itself to be `DuetReading<T>`. TypeScript's unions are
 * structural, so only the arm that actually holds a `T` is generic. The arm set,
 * the tags and the semantics are identical.
 */
export type DuetReading<T extends {}> = DuetPresent<T> | DuetNone | DuetAbsent | DuetMismatch;

/** Builds a present reading. */
export function duetPresent<T extends {}>(value: T): DuetPresent<T> {
  return { kind: 'present', value };
}

/** The one "explicitly null" reading. */
export function duetNone(): DuetNone {
  return { kind: 'none' };
}

/** The one "no such path" reading. */
export function duetAbsent(): DuetAbsent {
  return { kind: 'absent' };
}

/** Builds a mismatch reading. */
export function duetMismatch(found: DuetValue, reason: string): DuetMismatch {
  return { kind: 'mismatch', found, reason };
}

/**
 * The value, if `reading` is present; `null` otherwise.
 *
 * A convenience for the common "render it or render nothing" case. Prefer a
 * `switch` over `kind` where the three non-value outcomes differ.
 */
export function duetReadingValue<T extends {}>(reading: DuetReading<T>): T | null {
  return reading.kind === 'present' ? reading.value : null;
}

/**
 * A live typed subscription, returned by {@link DuetField.watch}.
 *
 * `current` is up to date the instant `watch` resolves: it already reflects the
 * host's subscription snapshot and every notification that raced ahead of the
 * reply. The callback fires only for changes *after* that point, so an
 * application never receives a notification for a handle it does not yet hold.
 */
export interface DuetWatch<T extends {}> {
  /** The most recent reading, updated before the callback for it runs. */
  readonly current: DuetReading<T>;

  /** True once {@link DuetWatch.close} has run. */
  readonly isClosed: boolean;

  /**
   * Cancels the subscription on the host and stops the callback.
   *
   * Idempotent. Rejects with whatever {@link DuetClient.unsubscribe} rejects
   * with, so a host that refuses the cancellation is not silently ignored.
   */
  close(): Promise<void>;
}

/**
 * Reads `value` as a required `T`.
 *
 * - `null` — no node at the path — is {@link DuetAbsent}.
 * - Anything the codec accepts is {@link DuetPresent}.
 * - Everything else, **including a `'null'` value**, is {@link DuetMismatch}: a
 *   required field promised a `T`, and a null is not one.
 */
export function duetRequiredReading<T extends {}>(
  codec: DuetCodec<T>,
  value: DuetValue | null,
): DuetReading<T> {
  if (value === null) return duetAbsent();
  return decodeOrMismatch(codec, value);
}

/**
 * Reads `value` as an optional `T`, mirroring Rust's `Option<T>`.
 *
 * - `null` — no node at the path — is {@link DuetAbsent}.
 * - A `'null'` value is {@link DuetNone}, without consulting the codec:
 *   `Option::None` lowers to `Value::Null` by definition, and no codec for a
 *   non-nullable `T` may claim it.
 * - Anything the codec accepts is {@link DuetPresent}; everything else is
 *   {@link DuetMismatch}.
 */
export function duetOptionalReading<T extends {}>(
  codec: DuetCodec<T>,
  value: DuetValue | null,
): DuetReading<T> {
  if (value === null) return duetAbsent();
  if (value.kind === 'null') return duetNone();
  return decodeOrMismatch(codec, value);
}

/**
 * Runs `codec` over `value`, turning every outcome into a reading.
 *
 * The `catch` is what makes this package's totality claim survive contact with
 * code it does not own. A codec is a **decoder**, and every decode path in this
 * package is total; a generated or hand-written codec that throws would
 * otherwise put an exception on the push path, where {@link DuetClient} hands it
 * straight to the transport's message handler.
 *
 * This is deliberately the *opposite* of {@link DuetClient.onPush}'s policy,
 * which lets an application handler's exception escape. The difference is whose
 * bug it is and whether it can be reported: a throwing codec is reported here,
 * as a mismatch carrying the thrown text, so it is visible rather than swallowed
 * — an application callback has no such channel, and hiding its exception would
 * hide a bug in the one place nobody would look.
 */
function decodeOrMismatch<T extends {}>(
  codec: DuetCodec<T>,
  value: DuetValue,
): DuetReading<T> {
  let decoded: T | null;
  try {
    decoded = codec.decode(value);
  } catch (error) {
    return duetMismatch(
      value,
      `the ${codec.name} codec threw: ${echoBounded(String(error))}`,
    );
  }
  if (decoded === null) {
    return duetMismatch(
      value,
      `expected ${codec.name}, found a value of kind ${echoBounded(value.kind)}`,
    );
  }
  return duetPresent(decoded);
}
