/// Typed `get`, `set` and `watch` at one fixed path.
library;

import '../duet_path.dart';
import '../duet_value.dart';
import 'duet_codec.dart';
import 'duet_reading.dart';
import 'duet_router.dart';

/// A typed handle on one path that always holds a `T`.
///
/// # The path is fixed at construction
///
/// It is parsed once, here, so a malformed path is a failure at wiring time
/// rather than on the first read. Generated code only ever passes a string
/// literal minted by the code generator, which makes
/// `DuetPath`'s unchecked `DuetPath(...)` constructor unreachable from
/// generated code — every path it produces has been through the parser.
///
/// # `T` is what the schema promises, not what the host guarantees
///
/// Another guest can write anything anywhere. A required field whose path
/// holds a `Value::Null`, or a string where the schema says an integer,
/// reports [DuetMismatch]; it does not throw. See [DuetReading].
final class DuetField<T extends Object> {
  /// Binds [path] on [router], decoding through [codec].
  ///
  /// Throws `DuetCodecException` with reason `bad_path` if [path] is not a
  /// legal path.
  DuetField(this.router, String path, this.codec) : path = DuetPath.parse(path);

  /// The router this field's watches are delivered through.
  final DuetRouter router;

  /// The parsed path this field addresses.
  final DuetPath path;

  /// The codec this field decodes and encodes through.
  final DuetCodec<T> codec;

  /// Reads the current value.
  ///
  /// Throws `DuetFailure` if the host refuses the read, and
  /// `DuetTransportException` if the exchange never reached the protocol. A
  /// value of the wrong type is **not** an exception; it is a [DuetMismatch].
  Future<DuetReading<T>> get() async =>
      duetRequiredReading<T>(codec, await router.client.get(path.toString()));

  /// Writes [value].
  ///
  /// Throws `DuetFailure` when the host refuses the write, carrying the host's
  /// own explanation. That is not a formality: writing to a path whose parent
  /// is absent or is a scalar genuinely fails on the host — `Value::set` never
  /// creates intermediate nodes — and this method surfaces that rather than
  /// quietly succeeding locally.
  Future<void> set(T value) =>
      router.client.set(path.toString(), codec.encode(value));

  /// Watches the path, calling [onReading] on every change after this future
  /// completes.
  ///
  /// The returned handle's `current` is already caught up when this returns;
  /// see `DuetRouter.watch`.
  Future<DuetWatch<T>> watch(void Function(DuetReading<T> reading) onReading) =>
      router.watch<T>(
        path,
        (DuetValue? value) => duetRequiredReading<T>(codec, value),
        onReading,
      );
}

/// A typed handle on one path that holds `Option<T>` — Rust's `Option<T>`,
/// lowered to `Value::Null` for `None`.
///
/// # Three outcomes, not two
///
/// `None` and "no such path" are different, and this class keeps them
/// different end to end:
///
/// - [DuetNone] — the path exists and holds `Value::Null`. This is `None`.
/// - [DuetAbsent] — there is no node at the path. The struct that would
///   contain it is itself `None`, or was never written.
///
/// Measured against the host with an `Option<Editor>` set to `None`, a child
/// path such as `editor.zoom` behaves three different ways at once: `get`
/// answers null, `subscribe` succeeds, and `set` **fails** with `path
/// "editor.zoom" addresses the wrong kind of node`. This API does not paper
/// over that — [get] reports [DuetAbsent], [watch] succeeds, and [set] throws
/// the host's refusal — because a `set` that silently no-ops is a lost write
/// the application never learns about.
final class DuetOptionalField<T extends Object> {
  /// Binds [path] on [router], decoding through [codec].
  ///
  /// Throws `DuetCodecException` with reason `bad_path` if [path] is not a
  /// legal path.
  DuetOptionalField(this.router, String path, this.codec)
      : path = DuetPath.parse(path);

  /// The router this field's watches are delivered through.
  final DuetRouter router;

  /// The parsed path this field addresses.
  final DuetPath path;

  /// The codec for the `T` inside the option.
  ///
  /// Non-nullable, like every [DuetCodec]. The optionality lives in this class
  /// rather than in the codec's type argument, which is exactly what keeps
  /// "decoded to null" from colliding with "refused" — see [DuetCodec].
  final DuetCodec<T> codec;

  /// Reads the current value.
  ///
  /// Throws `DuetFailure` if the host refuses the read. A wrong-typed value is
  /// a [DuetMismatch], not an exception.
  Future<DuetReading<T>> get() async =>
      duetOptionalReading<T>(codec, await router.client.get(path.toString()));

  /// Writes [value], or `Value::Null` — Rust's `None` — when it is null.
  ///
  /// There is no way to make the path *absent* again, and that is not an
  /// omission: the wire has no delete. `None` and "no such path" are different
  /// states, and only the host's own shape decides the second.
  ///
  /// Throws `DuetFailure` when the host refuses the write; see
  /// `DuetField.set`.
  Future<void> set(T? value) => router.client.set(
        path.toString(),
        value == null ? const DuetNull() : codec.encode(value),
      );

  /// Watches the path, calling [onReading] on every change after this future
  /// completes.
  Future<DuetWatch<T>> watch(void Function(DuetReading<T> reading) onReading) =>
      router.watch<T>(
        path,
        (DuetValue? value) => duetOptionalReading<T>(codec, value),
        onReading,
      );
}
