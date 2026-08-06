/**
 * Typed `get`, `set` and `watch` at one fixed path.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_field.dart`.
 *
 * @module
 */

import { formatDuetPath, parseDuetPath, type DuetPath } from '../path.ts';
import { duetNull, type DuetValue } from '../value.ts';
import type { DuetCodec } from './codec.ts';
import {
  duetOptionalReading,
  duetRequiredReading,
  type DuetReading,
  type DuetWatch,
} from './reading.ts';
import type { DuetRouter } from './router.ts';

/**
 * A typed handle on one path that always holds a `T`.
 *
 * # The path is fixed at construction
 *
 * It is parsed once, here, so a malformed path is a failure at wiring time
 * rather than on the first read. Generated code only ever passes a string
 * literal minted by the code generator, which means every path it produces has
 * been through the parser rather than assembled as an object literal.
 *
 * # `T` is what the schema promises, not what the host guarantees
 *
 * Another guest can write anything anywhere. A required field whose path holds a
 * `'null'` value, or a string where the schema says an integer, reports a
 * mismatch; it does not throw. See {@link DuetReading}.
 */
export class DuetField<T extends {}> {
  /** The router this field's watches are delivered through. */
  readonly router: DuetRouter;

  /** The parsed path this field addresses. */
  readonly path: DuetPath;

  /** The codec this field decodes and encodes through. */
  readonly codec: DuetCodec<T>;

  readonly #pathText: string;

  /**
   * Binds `path` on `router`, decoding through `codec`.
   *
   * @throws DuetCodecError with `bad_path` if `path` is not a legal path.
   */
  constructor(router: DuetRouter, path: string, codec: DuetCodec<T>) {
    this.router = router;
    this.path = parseDuetPath(path);
    this.codec = codec;
    this.#pathText = formatDuetPath(this.path);
  }

  /**
   * Reads the current value.
   *
   * A value of the wrong type is **not** an exception; it is a mismatch
   * reading.
   *
   * @throws DuetFailure if the host refuses the read.
   * @throws DuetTransportError if the exchange never reached the protocol.
   */
  async get(): Promise<DuetReading<T>> {
    return duetRequiredReading(this.codec, await this.router.client.get(this.#pathText));
  }

  /**
   * Writes `value`.
   *
   * Rejects with a `DuetFailure` when the host refuses the write, carrying the
   * host's own explanation. That is not a formality: writing to a path whose
   * parent is absent or is a scalar genuinely fails on the host — `Value::set`
   * never creates intermediate nodes — and this method surfaces that rather than
   * quietly succeeding locally.
   */
  set(value: T): Promise<void> {
    return this.router.client.set(this.#pathText, this.codec.encode(value));
  }

  /**
   * Watches the path, calling `onReading` on every change after this promise
   * resolves.
   *
   * The returned handle's `current` is already caught up when this resolves; see
   * {@link DuetRouter.watch}.
   */
  watch(onReading: (reading: DuetReading<T>) => void): Promise<DuetWatch<T>> {
    return this.router.watch<T>(
      this.path,
      (value: DuetValue | null) => duetRequiredReading(this.codec, value),
      onReading,
    );
  }
}

/**
 * A typed handle on one path that holds `Option<T>` — Rust's `Option<T>`,
 * lowered to `Value::Null` for `None`.
 *
 * # Three outcomes, not two
 *
 * `None` and "no such path" are different, and this class keeps them different
 * end to end:
 *
 * - a `'none'` reading — the path exists and holds `Value::Null`. This is
 *   `None`.
 * - an `'absent'` reading — there is no node at the path. The struct that would
 *   contain it is itself `None`, or was never written.
 *
 * Measured against the host with an `Option<Editor>` set to `None`, a child path
 * such as `editor.zoom` behaves three different ways at once: `get` answers
 * null, `subscribe` succeeds, and `set` **fails** with `path "editor.zoom"
 * addresses the wrong kind of node`. This API does not paper over that —
 * {@link DuetOptionalField.get} reports absent, {@link DuetOptionalField.watch}
 * succeeds, and {@link DuetOptionalField.set} rejects with the host's refusal —
 * because a `set` that silently no-ops is a lost write the application never
 * learns about.
 */
export class DuetOptionalField<T extends {}> {
  /** The router this field's watches are delivered through. */
  readonly router: DuetRouter;

  /** The parsed path this field addresses. */
  readonly path: DuetPath;

  /**
   * The codec for the `T` inside the option.
   *
   * Non-nullable, like every {@link DuetCodec}. The optionality lives in this
   * class rather than in the codec's type argument, which is exactly what keeps
   * "decoded to null" from colliding with "refused" — see {@link DuetCodec}.
   */
  readonly codec: DuetCodec<T>;

  readonly #pathText: string;

  /**
   * Binds `path` on `router`, decoding through `codec`.
   *
   * @throws DuetCodecError with `bad_path` if `path` is not a legal path.
   */
  constructor(router: DuetRouter, path: string, codec: DuetCodec<T>) {
    this.router = router;
    this.path = parseDuetPath(path);
    this.codec = codec;
    this.#pathText = formatDuetPath(this.path);
  }

  /**
   * Reads the current value.
   *
   * A wrong-typed value is a mismatch reading, not an exception.
   *
   * @throws DuetFailure if the host refuses the read.
   */
  async get(): Promise<DuetReading<T>> {
    return duetOptionalReading(this.codec, await this.router.client.get(this.#pathText));
  }

  /**
   * Writes `value`, or `Value::Null` — Rust's `None` — when it is null.
   *
   * There is no way to make the path *absent* again, and that is not an
   * omission: the wire has no delete. `None` and "no such path" are different
   * states, and only the host's own shape decides the second.
   *
   * Rejects with a `DuetFailure` when the host refuses the write; see
   * {@link DuetField.set}.
   */
  set(value: T | null): Promise<void> {
    return this.router.client.set(
      this.#pathText,
      value === null ? duetNull() : this.codec.encode(value),
    );
  }

  /**
   * Watches the path, calling `onReading` on every change after this promise
   * resolves.
   */
  watch(onReading: (reading: DuetReading<T>) => void): Promise<DuetWatch<T>> {
    return this.router.watch<T>(
      this.path,
      (value: DuetValue | null) => duetOptionalReading(this.codec, value),
      onReading,
    );
  }
}
