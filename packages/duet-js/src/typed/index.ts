/**
 * The typed runtime: a hand-written layer that turns Duet's untyped value tree
 * into typed fields and typed watchers.
 *
 * Import it separately from the package root:
 *
 * ```ts
 * import { DuetClient } from 'duet-protocol';
 * import { DuetRouter, DuetField, duetFloatCodec } from 'duet-protocol/typed';
 * ```
 *
 * A separate entry point, mirroring `package:duet/typed.dart` on the Dart side
 * and the existing `duet-protocol/wry` adapter, so that a guest that only speaks
 * the wire pays nothing for a layer it does not use.
 *
 * # What lives here and why it is hand-written
 *
 * Phase 4 generates typed clients from one Rust type definition. Everything the
 * generator will emit is declarations and literals; every *algorithm* those
 * declarations stand on lives here, written and tested by hand:
 *
 * - {@link duetValueAt} and {@link duetValueWith} — read and functionally update
 *   a value at a path, iteratively.
 * - {@link duetMergeMirror} — the three-case merge of one host patch into one
 *   watcher's mirror. The single hardest piece of logic in the layer, kept pure
 *   so it can be tested without a client.
 * - {@link DuetCodec} — the seam between a TypeScript type and a `DuetValue`.
 * - {@link DuetReading} and its four arms — what a field currently holds,
 *   including the two answers that are not a value and the one that is a
 *   disagreement between guests.
 * - {@link DuetRouter} — one owner of the client's push slot, routing
 *   notifications to watchers by subscription id.
 * - {@link DuetField} and {@link DuetOptionalField} — typed `get`/`set`/`watch`
 *   at one fixed path.
 *
 * It is useful on its own: a guest with no generated code at all can build
 * fields by hand and get a mirror that stays correct under concurrent writes
 * from another guest.
 *
 * @module
 */

export { duetPathSuffix, duetValueAt, duetValueWith } from './value-path.ts';

export {
  duetMerged,
  duetMergeMirror,
  duetResync,
  type DuetMerge,
  type DuetMerged,
  type DuetResync,
} from './merge.ts';

export {
  duetBoolCodec,
  duetBytesCodec,
  duetFloatCodec,
  duetIntCodec,
  duetListCodec,
  duetMapCodec,
  duetStringCodec,
  type DuetCodec,
} from './codec.ts';

export {
  duetAbsent,
  duetMismatch,
  duetNone,
  duetOptionalReading,
  duetPresent,
  duetReadingValue,
  duetRequiredReading,
  type DuetAbsent,
  type DuetMismatch,
  type DuetNone,
  type DuetPresent,
  type DuetReading,
  type DuetWatch,
} from './reading.ts';

export { DuetRouter, DEFAULT_MAX_BUFFERED_PUSHES } from './router.ts';

export { DuetField, DuetOptionalField } from './field.ts';
