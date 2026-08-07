/**
 * The typed half of an `invoke`: {@link DuetOutcome} and
 * {@link duetDecodeOutcome}.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_outcome.dart`.
 *
 * @module
 */

import type { DuetInvocation } from '../client.ts';
import type { DuetValue } from '../value.ts';
import type { DuetCodec } from './codec.ts';

/**
 * The command returned `Ok`, and this client decoded it.
 *
 * A command whose schema declares no `returns` still lands here, with `value`
 * the null the host sends — decoded through `duetDynamicCodec`, whose type
 * argument is {@link DuetValue}.
 */
export interface DuetOk<T extends {}> {
  readonly kind: 'ok';
  /** What the command returned. */
  readonly value: T;
}

/** The command returned `Err`, and this client decoded it. */
export interface DuetErr<E extends {}> {
  readonly kind: 'err';
  /** The domain error the command returned. */
  readonly error: E;
}

/**
 * The command answered, and the codec its schema calls for refused the answer.
 *
 * `raised` says which of the two replies it was, because the raw value alone
 * cannot: a `returned` that did not decode and a `raised` that did not decode
 * are different events, and only the first means the call may have succeeded.
 */
export interface DuetUndecodable {
  readonly kind: 'undecodable';
  /** Exactly what the host sent, tagged as it arrived. */
  readonly value: DuetValue;
  /** True if the host answered `raised`, false if it answered `returned`. */
  readonly raised: boolean;
}

/**
 * What a command that **ran** produced, decoded through its schema types.
 *
 * {@link DuetInvocation} is the untyped answer: two arms carrying a
 * {@link DuetValue} each. This is the same two arms with the values decoded,
 * plus a third for the answer that did not decode at all.
 *
 * # Why the third arm exists
 *
 * A generated client binds the codecs the *schema* declares. The host is a
 * separate process that may be older, newer, or simply wrong, so "the command
 * returned something this codec cannot read" is a state the wire can genuinely
 * be in — exactly as `DuetMismatch` is for a path. Collapsing it into a thrown
 * error would make an ordinary version skew look like a bug in the caller;
 * collapsing it into `null` would make it indistinguishable from a command that
 * returned nothing. So it is an arm, and it carries the raw value so an
 * application can log or salvage it.
 *
 * # This is not where a refusal lands
 *
 * A host that would not run the command at all rejects `DuetClient.invoke` with
 * a `DuetFailure`, and nothing here sees it. That split is
 * {@link DuetInvocation}'s, and this type inherits it unchanged: every arm above
 * describes a body that executed.
 *
 * # Where this diverges from the Dart mirror, and why
 *
 * Dart's version is a `sealed` class hierarchy carrying both type arguments on
 * every arm; TypeScript's unions are structural, so `DuetOk` needs only `T` and
 * `DuetErr` only `E`. Same arms, same tags, same semantics — the identical
 * divergence `DuetReading` already documents.
 *
 * # The bound is `{}`, not `unknown`
 *
 * {@link DuetCodec}'s type argument is non-nullable by design — that is what
 * keeps "decoded to null" from colliding with "refused" — and these two
 * arguments are that codec's, so they carry the same bound. A command with
 * nothing declared at a position is generated with `duetDynamicCodec`, whose
 * argument is {@link DuetValue}: still an object, and still not `null`.
 */
export type DuetOutcome<T extends {}, E extends {}> =
  | DuetOk<T>
  | DuetErr<E>
  | DuetUndecodable;

/**
 * Decodes one {@link DuetInvocation} through the two codecs its schema declares.
 *
 * The whole algorithm behind a generated command method, kept here so the
 * generated code is declarations and literals only — the same rule
 * `duetRequiredReading` exists under for fields.
 *
 * `returns` decodes a `returned` and `raises` decodes a `raised`; neither is
 * ever applied to the other's arm. A command that declares no `returns` (or no
 * `raises`) is generated with `duetDynamicCodec` there, which decodes anything,
 * so the {@link DuetUndecodable} arm cannot fire for a type the schema never
 * described.
 */
export function duetDecodeOutcome<T extends {}, E extends {}>(
  invocation: DuetInvocation,
  returns: DuetCodec<T>,
  raises: DuetCodec<E>,
): DuetOutcome<T, E> {
  if (invocation.kind === 'returned') {
    const decoded = returns.decode(invocation.value);
    return decoded === null
      ? { kind: 'undecodable', value: invocation.value, raised: false }
      : { kind: 'ok', value: decoded };
  }
  const decoded = raises.decode(invocation.error);
  return decoded === null
    ? { kind: 'undecodable', value: invocation.error, raised: true }
    : { kind: 'err', error: decoded };
}
