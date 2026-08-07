/// The typed half of an `invoke`: [DuetOutcome] and [duetDecodeOutcome].
library;

import '../duet_client.dart';
import '../duet_value.dart';
import 'duet_codec.dart';

/// What a command that **ran** produced, decoded through its schema types.
///
/// [DuetInvocation] is the untyped answer: two arms carrying a [DuetValue]
/// each. This is the same two arms with the values decoded, plus a third for
/// the answer that did not decode at all.
///
/// # Why the third arm exists
///
/// A generated client binds the codecs the *schema* declares. The host is a
/// separate process that may be older, newer, or simply wrong, so "the command
/// returned something this codec cannot read" is a state the wire can genuinely
/// be in — exactly as `DuetMismatch` is for a path. Collapsing it into a
/// thrown exception would make an ordinary version skew look like a bug in the
/// caller; collapsing it into `null` would make it indistinguishable from a
/// command that returned nothing. So it is an arm, and it carries the raw value
/// so an application can log or salvage it.
///
/// # What a caller must do with it
///
/// `switch` over it. The hierarchy is `sealed`, so a caller that handles only
/// [DuetOk] fails to compile rather than silently treating a command's error as
/// a success.
///
/// # This is not where a refusal lands
///
/// A host that would not run the command at all throws `DuetFailure` out of
/// [DuetClient.invoke], and nothing here sees it. That split is
/// [DuetInvocation]'s, and this type inherits it unchanged: every arm below
/// describes a body that executed.
sealed class DuetOutcome<T extends Object, E extends Object> {
  /// Allows subclasses to be `const`.
  const DuetOutcome();
}

/// The command returned `Ok`, and this client decoded it.
///
/// A command whose schema declares no `returns` still lands here, with `value`
/// the [DuetNull] the host sends — decoded through `duetDynamicCodec`, whose
/// type argument is [DuetValue].
final class DuetOk<T extends Object, E extends Object> extends DuetOutcome<T, E> {
  /// Wraps the decoded result.
  const DuetOk(this.value);

  /// What the command returned.
  final T value;

  @override
  bool operator ==(Object other) => other is DuetOk<T, E> && other.value == value;

  @override
  int get hashCode => Object.hash('ok', value);

  @override
  String toString() => 'Ok($value)';
}

/// The command returned `Err`, and this client decoded it.
final class DuetErr<T extends Object, E extends Object> extends DuetOutcome<T, E> {
  /// Wraps the decoded error.
  const DuetErr(this.error);

  /// The domain error the command returned.
  final E error;

  @override
  bool operator ==(Object other) => other is DuetErr<T, E> && other.error == error;

  @override
  int get hashCode => Object.hash('err', error);

  @override
  String toString() => 'Err($error)';
}

/// The command answered, and the codec its schema calls for refused the answer.
///
/// [raised] says which of the two replies it was, because the raw value alone
/// cannot: a `returned` that did not decode and a `raised` that did not decode
/// are different events, and only the first means the call may have succeeded.
final class DuetUndecodable<T extends Object, E extends Object>
    extends DuetOutcome<T, E> {
  /// Wraps the answer no codec here could read.
  const DuetUndecodable({required this.value, required this.raised});

  /// Exactly what the host sent, tagged as it arrived.
  final DuetValue value;

  /// True if the host answered `raised`, false if it answered `returned`.
  final bool raised;

  @override
  bool operator ==(Object other) =>
      other is DuetUndecodable<T, E> && other.value == value && other.raised == raised;

  @override
  int get hashCode => Object.hash('undecodable', value, raised);

  @override
  String toString() => 'Undecodable($value, raised: $raised)';
}

/// Decodes one [DuetInvocation] through the two codecs its schema declares.
///
/// The whole algorithm behind a generated command method, kept here so the
/// generated code is declarations and literals only — the same rule
/// `duetRequiredReading` exists under for fields.
///
/// [returns] decodes a [DuetReturned] and [raises] decodes a [DuetRaised];
/// neither is ever applied to the other's arm. A command that declares no
/// `returns` (or no `raises`) is generated with `duetDynamicCodec` there, which
/// decodes anything, so the [DuetUndecodable] arm cannot fire for a type the
/// schema never described.
DuetOutcome<T, E> duetDecodeOutcome<T extends Object, E extends Object>(
  DuetInvocation invocation,
  DuetCodec<T> returns,
  DuetCodec<E> raises,
) {
  switch (invocation) {
    case DuetReturned(:final DuetValue value):
      final T? decoded = returns.decode(value);
      return decoded == null
          ? DuetUndecodable<T, E>(value: value, raised: false)
          : DuetOk<T, E>(decoded);
    case DuetRaised(:final DuetValue error):
      final E? decoded = raises.decode(error);
      return decoded == null
          ? DuetUndecodable<T, E>(value: error, raised: true)
          : DuetErr<T, E>(decoded);
  }
}
