/// The seam between a Dart type and the host's [DuetValue] tree.
library;

import '../duet_value.dart';

/// Encodes and decodes one Dart type to and from a [DuetValue].
///
/// Generated code implements this once per schema type; the primitive codecs
/// below cover the leaves.
///
/// # The non-nullable bound is load-bearing, not stylistic
///
/// [decode] answers `null` for "this value is not a `T`". That spelling is
/// only unambiguous because `T` itself can never *be* null: with a nullable
/// type argument, `DuetCodec<String?>.decode` returning `null` would mean
/// either "refused" or "decoded, and the answer is null", and nothing at the
/// call site could tell the two apart.
///
/// That ambiguity is not cosmetic here. The whole client is built on the
/// distinction between a path that holds [DuetNull] and a path that does not
/// exist — `DuetClient.get` returns `null` for the second and a [DuetNull] for
/// the first, and the wire spends a distinct spelling on each
/// (`DuetValue.optionalFromJson`). A codec that could not tell "refused" from
/// "null" would collapse exactly that distinction at the one layer whose job
/// is to preserve it.
///
/// `T extends Object` rejects a nullable type argument **at the definition
/// site**, so no such codec can be written at all. An optional field is
/// expressed by pairing a non-nullable codec with `DuetOptionalField`, which
/// keeps "absent", "null" and "a `T`" as three separate outcomes.
abstract interface class DuetCodec<T extends Object> {
  /// The name of the type this codec decodes, for error messages.
  ///
  /// Used to build the `reason` on a `DuetMismatch`, which is the only account
  /// an application gets of a type disagreement between two guests sharing one
  /// store. `'String'`, `'Editor'`, `'List<int>'`.
  String get name;

  /// Encodes [value] for the wire. Must be total: an encoder that can throw
  /// turns a typed `set` into a crash on data the application already holds.
  DuetValue encode(T value);

  /// Decodes [value], or answers `null` if it is not a `T`.
  ///
  /// Should be total. A throw is *tolerated* — `duetRequiredReading` and
  /// `duetOptionalReading` catch it and report a mismatch — but it costs the
  /// caller the diagnosis a `null` plus [name] would have given.
  T? decode(DuetValue value);
}

/// A codec for `bool`, mirroring Rust's `bool` and `Value::Bool`.
const DuetCodec<bool> duetBoolCodec = _BoolCodec();

/// A codec for `int`, mirroring Rust's `i64` and `Value::Int`.
const DuetCodec<int> duetIntCodec = _IntCodec();

/// A codec for `double`, mirroring Rust's `f64` and `Value::Float`.
///
/// Accepts only [DuetFloat]. An integer on the wire is **not** widened to a
/// double: `Value::Int` and `Value::Float` are distinct host types, and a
/// codec that quietly accepted one for the other would let a guest write an
/// `i64` where the schema says `f64` and have every other guest agree — the
/// exact cross-guest type drift `DuetMismatch` exists to surface.
const DuetCodec<double> duetFloatCodec = _FloatCodec();

/// A codec for `String`, mirroring Rust's `String` and `Value::Str`.
const DuetCodec<String> duetStringCodec = _StringCodec();

/// A codec for raw bytes, mirroring Rust's `Vec<u8>` and `Value::Bytes`.
///
/// Distinct from [duetStringCodec] even though both travel as a JSON string:
/// the wire tags them apart, and so does this.
const DuetCodec<List<int>> duetBytesCodec = _BytesCodec();

/// A codec for a homogeneous list, mirroring Rust's `Vec<T>` and `Value::List`.
///
/// Refuses the whole list if any element is refused, rather than dropping the
/// bad element: a partial list is a wrong answer that looks like a right one.
DuetCodec<List<T>> duetListCodec<T extends Object>(DuetCodec<T> item) =>
    _ListCodec<T>(item);

/// A codec for a string-keyed map, mirroring Rust's `BTreeMap<String, T>` and
/// `Value::Map`.
///
/// Refuses the whole map if any value is refused, for the same reason
/// [duetListCodec] does.
DuetCodec<Map<String, T>> duetMapCodec<T extends Object>(DuetCodec<T> value) =>
    _MapCodec<T>(value);

final class _BoolCodec implements DuetCodec<bool> {
  const _BoolCodec();

  @override
  String get name => 'bool';

  @override
  DuetValue encode(bool value) => DuetBool(value);

  @override
  bool? decode(DuetValue value) => value is DuetBool ? value.value : null;
}

final class _IntCodec implements DuetCodec<int> {
  const _IntCodec();

  @override
  String get name => 'int';

  @override
  DuetValue encode(int value) => DuetInt(value);

  @override
  int? decode(DuetValue value) => value is DuetInt ? value.value : null;
}

final class _FloatCodec implements DuetCodec<double> {
  const _FloatCodec();

  @override
  String get name => 'double';

  @override
  DuetValue encode(double value) => DuetFloat(value);

  @override
  double? decode(DuetValue value) => value is DuetFloat ? value.value : null;
}

final class _StringCodec implements DuetCodec<String> {
  const _StringCodec();

  @override
  String get name => 'String';

  @override
  DuetValue encode(String value) => DuetStr(value);

  @override
  String? decode(DuetValue value) => value is DuetStr ? value.value : null;
}

final class _BytesCodec implements DuetCodec<List<int>> {
  const _BytesCodec();

  @override
  String get name => 'bytes';

  @override
  DuetValue encode(List<int> value) => DuetBytes(value);

  @override
  List<int>? decode(DuetValue value) => value is DuetBytes ? value.value : null;
}

final class _ListCodec<T extends Object> implements DuetCodec<List<T>> {
  const _ListCodec(this._item);

  final DuetCodec<T> _item;

  @override
  String get name => 'List<${_item.name}>';

  @override
  DuetValue encode(List<T> value) =>
      DuetList(<DuetValue>[for (final T item in value) _item.encode(item)]);

  @override
  List<T>? decode(DuetValue value) {
    if (value is! DuetList) return null;
    final List<T> out = <T>[];
    for (final DuetValue item in value.items) {
      final T? decoded = _item.decode(item);
      if (decoded == null) return null;
      out.add(decoded);
    }
    return out;
  }
}

final class _MapCodec<T extends Object> implements DuetCodec<Map<String, T>> {
  const _MapCodec(this._value);

  final DuetCodec<T> _value;

  @override
  String get name => 'Map<String, ${_value.name}>';

  @override
  DuetValue encode(Map<String, T> value) => DuetMap(<String, DuetValue>{
        for (final MapEntry<String, T> e in value.entries)
          e.key: _value.encode(e.value),
      });

  @override
  Map<String, T>? decode(DuetValue value) {
    if (value is! DuetMap) return null;
    final Map<String, T> out = <String, T>{};
    for (final MapEntry<String, DuetValue> e in value.entries.entries) {
      final T? decoded = _value.decode(e.value);
      if (decoded == null) return null;
      out[e.key] = decoded;
    }
    return out;
  }
}
