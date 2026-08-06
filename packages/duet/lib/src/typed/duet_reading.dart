/// What a typed field currently holds — including the two answers that are
/// not a value.
library;

import '../duet_error.dart';
import '../duet_value.dart';
import 'duet_codec.dart';

/// One typed observation of one path.
///
/// # Why a sealed result and not an exception
///
/// A type mismatch is a **state the host can be in**, not a failure of the
/// call that found it. Another guest can write any value to any path — the
/// two-guest proof has a webview and a Flutter engine writing one store at the
/// same time — so a typed watcher will eventually meet a value its codec
/// refuses. That is data, and it arrives through a push, where there is no
/// call stack to throw into and where throwing would take out the transport's
/// message handler.
///
/// Given that `watch` cannot throw, `get` **must not** throw either. Two
/// mechanisms for one condition would mean an application that handles a
/// mismatch on its watcher still crashes the first time it reads the same path
/// directly. One sealed type used by `get`, by `watch` and by the router's own
/// mirror is the smaller API: a reader learns it once, and Dart's
/// exhaustiveness checking on `switch` makes forgetting an arm a compile
/// error rather than a runtime surprise.
///
/// # Why four arms
///
/// Because the host can be in exactly four states, and the wire already spends
/// a distinct spelling on each: a decodable value, `{"t":"n"}`, a `null` where
/// a value would go, and a value of some other type. Collapsing any two would
/// delete a distinction the protocol pays for — the same argument
/// `DuetValue.optionalFromJson` makes one layer down.
sealed class DuetReading<T extends Object> {
  const DuetReading();

  /// The value, if this reading is a [DuetPresent]; `null` otherwise.
  ///
  /// A convenience for the common "render it or render nothing" case. Prefer
  /// an exhaustive `switch` where the three non-value outcomes differ.
  T? get valueOrNull => switch (this) {
        DuetPresent<T>(:final T value) => value,
        _ => null,
      };
}

/// The path holds a value the codec accepted.
final class DuetPresent<T extends Object> extends DuetReading<T> {
  /// Records the decoded [value].
  const DuetPresent(this.value);

  /// The decoded value.
  final T value;

  @override
  bool operator ==(Object other) => other is DuetPresent<T> && other.value == value;

  @override
  int get hashCode => value.hashCode;

  @override
  String toString() => 'Present($value)';
}

/// The path exists and holds [DuetNull] — Rust's `Option::None`.
///
/// Only `DuetOptionalField` produces this. A `DuetField` whose schema promises
/// a `T` reports a [DuetMismatch] for a null, because a null is not the `T` it
/// was promised.
final class DuetNone<T extends Object> extends DuetReading<T> {
  /// The one "explicitly null" reading.
  const DuetNone();

  @override
  bool operator ==(Object other) => other is DuetNone<T>;

  @override
  int get hashCode => (DuetNone<T>).hashCode;

  @override
  String toString() => 'None';
}

/// There is no node at the path at all.
///
/// Distinct from [DuetNone]: nothing was ever written here, or an ancestor of
/// this path is a scalar, or a list index is out of range. The host says this
/// with a `null` where a value would go; `DuetClient.get` says it with `null`.
final class DuetAbsent<T extends Object> extends DuetReading<T> {
  /// The one "no such path" reading.
  const DuetAbsent();

  @override
  bool operator ==(Object other) => other is DuetAbsent<T>;

  @override
  int get hashCode => (DuetAbsent<T>).hashCode;

  @override
  String toString() => 'Absent';
}

/// The path holds a value, and the codec refused it.
///
/// The honest report of two guests disagreeing about one path's type. Carries
/// what was actually found so an application can log it, show it, or write the
/// right type back over it.
final class DuetMismatch<T extends Object> extends DuetReading<T> {
  /// Records the offending [found] value and a developer-facing [reason].
  const DuetMismatch(this.found, this.reason);

  /// The value the host actually holds at the path.
  final DuetValue found;

  /// Why the codec refused it, already bounded by [maxEchoChars] wherever it
  /// embeds host-supplied text.
  final String reason;

  @override
  bool operator ==(Object other) =>
      other is DuetMismatch<T> && other.found == found && other.reason == reason;

  @override
  int get hashCode => Object.hash(found, reason);

  @override
  String toString() => 'Mismatch($reason)';
}

/// A live typed subscription, returned by `DuetField.watch`.
///
/// [current] is up to date the instant `watch` returns: it already reflects
/// the host's subscription snapshot and every notification that raced ahead of
/// the reply. The callback fires only for changes *after* that point, so an
/// application never receives a notification for a handle it does not yet
/// hold.
abstract interface class DuetWatch<T extends Object> {
  /// The most recent reading, updated before the callback for it runs.
  DuetReading<T> get current;

  /// True once [close] has run.
  bool get isClosed;

  /// Cancels the subscription on the host and stops the callback.
  ///
  /// Idempotent. Propagates whatever `DuetClient.unsubscribe` throws, so a
  /// host that refuses the cancellation is not silently ignored.
  Future<void> close();
}

/// Reads [value] as a required `T`.
///
/// - `null` — no node at the path — is [DuetAbsent].
/// - Anything the codec accepts is [DuetPresent].
/// - Everything else, **including [DuetNull]**, is [DuetMismatch]: a required
///   field promised a `T`, and a null is not one.
DuetReading<T> duetRequiredReading<T extends Object>(
  DuetCodec<T> codec,
  DuetValue? value,
) {
  if (value == null) return DuetAbsent<T>();
  return _decodeOrMismatch(codec, value);
}

/// Reads [value] as an optional `T`, mirroring Rust's `Option<T>`.
///
/// - `null` — no node at the path — is [DuetAbsent].
/// - [DuetNull] is [DuetNone], without consulting the codec: `Option::None`
///   lowers to `Value::Null` by definition, and no codec for a non-nullable
///   `T` may claim it.
/// - Anything the codec accepts is [DuetPresent]; everything else is
///   [DuetMismatch].
DuetReading<T> duetOptionalReading<T extends Object>(
  DuetCodec<T> codec,
  DuetValue? value,
) {
  if (value == null) return DuetAbsent<T>();
  if (value is DuetNull) return DuetNone<T>();
  return _decodeOrMismatch(codec, value);
}

/// Runs [codec] over [value], turning every outcome into a reading.
///
/// The `catch` is what makes this package's totality claim survive contact
/// with code it does not own. A codec is a **decoder**, and every decode path
/// in this package is total; a generated or hand-written codec that throws
/// would otherwise put an exception on the push path, where `DuetClient`
/// hands it straight to the transport's message handler.
///
/// This is deliberately the *opposite* of `DuetClient.onPush`'s policy, which
/// lets an application handler's exception escape. The difference is whose bug
/// it is and whether it can be reported: a throwing codec is reported here, as
/// a mismatch carrying the thrown text, so it is visible rather than swallowed
/// — an application callback has no such channel, and hiding its exception
/// would hide a bug in the one place nobody would look.
DuetReading<T> _decodeOrMismatch<T extends Object>(
  DuetCodec<T> codec,
  DuetValue value,
) {
  final T? decoded;
  try {
    decoded = codec.decode(value);
  } on Object catch (error) {
    return DuetMismatch<T>(
      value,
      'the ${codec.name} codec threw: ${echoBounded(error.toString())}',
    );
  }
  if (decoded == null) {
    return DuetMismatch<T>(
      value,
      'expected ${codec.name}, found ${echoBounded(value.toString())}',
    );
  }
  return DuetPresent<T>(decoded);
}
