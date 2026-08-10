/// The Duet value tree and its tagged-JSON codec.
///
/// Mirrors `duet_core::Value` (crates/duet-core/src/value.rs) and
/// `duet-codec`'s encoding of it (crates/duet-codec/src/value.rs).
library;

import 'dart:convert';
import 'dart:typed_data';

import 'duet_error.dart';
import 'duet_json.dart';

/// One value in the host's shared state tree.
///
/// A sealed class rather than an enum: each variant carries different data (a
/// `bool`, an `int`, a `List<DuetValue>`, ...), which Dart enums cannot
/// express. `sealed` still gives exhaustiveness checking on `switch`.
sealed class DuetValue {
  const DuetValue();

  /// The tagged JSON form: a `Map<String, Object?>` ready for [encodeDuetJson],
  /// with its keys already in canonical order.
  Map<String, Object?> toJson();

  /// This value as the exact text the wire carries.
  String toWireText() => encodeDuetJson(toJson());

  /// Decodes one tagged value from wire text.
  ///
  /// Throws [DuetCodecException] and nothing else, whatever [text] contains —
  /// including text that is not JSON, and text nested past [maxJsonDepth].
  static DuetValue fromWireText(String text) => fromJson(decodeDuetJson(text));

  /// Decodes one tagged value from already-parsed JSON.
  ///
  /// Total over all JSON input: every branch either returns a [DuetValue] or
  /// throws [DuetCodecException] — never a bare type-cast failure, which would
  /// surface as an unrelated `TypeError` instead of a diagnosable message
  /// naming the field and the tag.
  static DuetValue fromJson(Object? json) => _fromJson(json, 1);

  /// JSON `null` means "no value at this path" — distinct from `{"t":"n"}`,
  /// which is [DuetNull]. The wire draws the same distinction; a client that
  /// collapsed the two would lose it. See crates/duet-protocol/src/wire.rs's
  /// `optional_value`.
  static DuetValue? optionalFromJson(Object? json) =>
      json == null ? null : fromJson(json);

  /// The recursion, carrying the number of JSON containers enclosing [json]
  /// (counting the tagged object itself).
  ///
  /// [decodeDuetJson] already refuses over-deep wire text, so this bound fires
  /// only when a caller hands an over-deep tree straight to [fromJson]. It is
  /// kept anyway: an unbounded recursive decoder is a stack overflow waiting
  /// for the first caller who forgets, and a stack overflow is not catchable.
  static DuetValue _fromJson(Object? json, int depth) {
    if (depth > maxJsonDepth) {
      throw DuetCodecException.badJson(
        'value nests deeper than $maxJsonDepth JSON containers',
      );
    }
    if (json is! Map<String, Object?>) {
      throw DuetCodecException.badShape(
        'expected an object, found ${jsonTypeName(json)}',
      );
    }
    final Object? tag = json['t'];
    if (tag == null) throw DuetCodecException.badShape('missing "t"');
    if (tag is! String) {
      throw DuetCodecException.badShape('"t" must be a string');
    }

    if (tag == 'n') return const DuetNull();

    // Each arm reads "v" for itself, and only once the arm for that specific
    // tag has matched, so an unrecognised tag is refused without the decoder
    // ever going looking for a payload that would not have helped. Mirrors
    // duet-codec's `decode_value`.
    Object? payload() {
      if (!json.containsKey('v')) {
        throw DuetCodecException.badShape('tag "$tag" requires "v"');
      }
      return json['v'];
    }

    switch (tag) {
      case 'bool':
        final Object? v = payload();
        if (v is! bool) {
          throw DuetCodecException.badShape('"bool" payload must be a boolean');
        }
        return DuetBool(v);
      case 'i':
        return DuetInt(_decodeInt(payload()));
      case 'f':
        return DuetFloat(_decodeFloat(payload()));
      case 's':
        final Object? v = payload();
        if (v is! String) {
          throw DuetCodecException.badShape('"s" payload must be a string');
        }
        return DuetStr(v);
      case 'b':
        return DuetBytes(_decodeBytes(payload()));
      case 'l':
        final Object? v = payload();
        if (v is! List<Object?>) {
          throw DuetCodecException.badShape('"l" payload must be an array');
        }
        return DuetList(<DuetValue>[
          for (final Object? item in v) _fromJson(item, depth + 2),
        ]);
      case 'm':
        final Object? v = payload();
        if (v is! Map<String, Object?>) {
          throw DuetCodecException.badShape('"m" payload must be an object');
        }
        return DuetMap(<String, DuetValue>{
          for (final MapEntry<String, Object?> e in v.entries)
            e.key: _fromJson(e.value, depth + 2),
        });
      default:
        throw DuetCodecException.unknownTag(
          'unknown type tag ${echoBounded(tag)}',
        );
    }
  }
}

/// The absence of a value, distinct from a path that does not exist at all
/// (see [DuetValue.optionalFromJson]).
final class DuetNull extends DuetValue {
  /// The one null value.
  const DuetNull();

  @override
  Map<String, Object?> toJson() => const <String, Object?>{'t': 'n'};

  @override
  bool operator ==(Object other) => other is DuetNull;

  @override
  int get hashCode => 0;

  @override
  String toString() => 'Null';
}

/// A boolean.
final class DuetBool extends DuetValue {
  /// Wraps [value].
  const DuetBool(this.value);

  /// The wrapped boolean.
  final bool value;

  @override
  Map<String, Object?> toJson() => <String, Object?>{'t': 'bool', 'v': value};

  @override
  bool operator ==(Object other) => other is DuetBool && other.value == value;

  @override
  int get hashCode => value.hashCode;

  @override
  String toString() => 'Bool($value)';
}

/// A signed 64-bit integer.
final class DuetInt extends DuetValue {
  /// Wraps [value].
  const DuetInt(this.value);

  /// The wrapped signed 64-bit integer.
  final int value;

  /// Encodes the payload as a decimal **string**, never a JSON number
  /// (crates/duet-codec/src/value.rs's `encode_value`).
  ///
  /// A JSON number above 2^53 loses precision once it round-trips through an
  /// IEEE-754 double, which is what JSON numbers are in both Dart's `num` and
  /// JavaScript. A decimal string sidesteps that in both directions.
  ///
  /// `int.toString()` is already canonical — no `+`, no leading zeros, no
  /// `-0` — which is what `duet_codec::is_canonical_signed_decimal` requires.
  @override
  Map<String, Object?> toJson() =>
      <String, Object?>{'t': 'i', 'v': value.toString()};

  @override
  bool operator ==(Object other) => other is DuetInt && other.value == value;

  @override
  int get hashCode => value.hashCode;

  @override
  String toString() => 'Int($value)';
}

/// A 64-bit float, including NaN and both infinities.
final class DuetFloat extends DuetValue {
  /// Wraps [value].
  const DuetFloat(this.value);

  /// The wrapped double, which may be NaN or either infinity.
  final double value;

  /// The four doubles with no portable JSON-number spelling travel as string
  /// sentinels (crates/duet-codec/src/value.rs's `encode_float`).
  ///
  /// `jsonEncode` **throws** on NaN and both infinities, so those three are
  /// mandatory here, not cosmetic. `-0.0` is different: Dart *can* write it as
  /// a JSON number and `jsonEncode` preserves the sign, but JavaScript cannot
  /// (`JSON.stringify(-0)` is `"0"`), so the sentinel exists for the JS guest
  /// and all three implementations emit it — the wire must have one spelling
  /// per value, not one per language.
  ///
  /// `value.isNegative` is the test, not `value == -0.0`: `-0.0 == 0.0` is
  /// true in Dart exactly as in IEEE 754, so an equality check would tag every
  /// zero as negative.
  @override
  Map<String, Object?> toJson() => <String, Object?>{
        't': 'f',
        'v': value.isNaN
            ? 'NaN'
            : value == double.infinity
                ? 'Infinity'
                : value == double.negativeInfinity
                    ? '-Infinity'
                    : (value == 0.0 && value.isNegative)
                        ? '-0'
                        : value,
      };

  /// Compares with `==` on the wrapped double, mirroring Rust's derived
  /// `PartialEq` on `Value::Float(f64)`.
  ///
  /// That means IEEE-754 semantics leak through: `DuetFloat(nan)` equals
  /// nothing, not even itself, and `DuetFloat(-0.0)` equals `DuetFloat(0.0)`
  /// despite encoding differently. This is why the golden corpus compares
  /// floats by their **bits** rather than by equality — a test written with
  /// `==` would be vacuous for every NaN case and blind to a lost sign bit.
  @override
  bool operator ==(Object other) => other is DuetFloat && other.value == value;

  @override
  int get hashCode => value.hashCode;

  @override
  String toString() => 'Float($value)';
}

/// A string.
final class DuetStr extends DuetValue {
  /// Wraps [value].
  const DuetStr(this.value);

  /// The wrapped string.
  final String value;

  @override
  Map<String, Object?> toJson() => <String, Object?>{'t': 's', 'v': value};

  @override
  bool operator ==(Object other) => other is DuetStr && other.value == value;

  @override
  int get hashCode => value.hashCode;

  @override
  String toString() => 'Str($value)';
}

/// Raw bytes.
///
/// Kept distinguishable from [DuetStr] by its own tag even though both encode
/// to a JSON string — the single clearest reason the encoding is tagged at all.
final class DuetBytes extends DuetValue {
  /// Wraps [value]. The list is not copied; treat it as owned by this value.
  const DuetBytes(this.value);

  /// The wrapped raw bytes.
  final List<int> value;

  @override
  Map<String, Object?> toJson() =>
      <String, Object?>{'t': 'b', 'v': base64.encode(value)};

  @override
  bool operator ==(Object other) {
    if (other is! DuetBytes || other.value.length != value.length) return false;
    for (int i = 0; i < value.length; i++) {
      if (other.value[i] != value[i]) return false;
    }
    return true;
  }

  @override
  int get hashCode => Object.hashAll(value);

  @override
  String toString() => 'Bytes(${value.length} bytes)';
}

/// An ordered list of values.
final class DuetList extends DuetValue {
  /// Wraps [items]. The list is not copied; treat it as owned by this value.
  const DuetList(this.items);

  /// The wrapped, order-preserving list of values.
  final List<DuetValue> items;

  @override
  Map<String, Object?> toJson() => <String, Object?>{
        't': 'l',
        'v': <Object?>[for (final DuetValue item in items) item.toJson()],
      };

  @override
  bool operator ==(Object other) {
    if (other is! DuetList || other.items.length != items.length) return false;
    for (int i = 0; i < items.length; i++) {
      if (other.items[i] != items[i]) return false;
    }
    return true;
  }

  @override
  int get hashCode => Object.hashAll(items);

  @override
  String toString() => 'List($items)';
}

/// A string-keyed map of values.
///
/// [entries] is an ordinary Dart map, so it iterates in *insertion* order.
/// [toJson] therefore sorts — see [compareDuetMapKeys] for the canonical order
/// and why it is not `String.compareTo`.
final class DuetMap extends DuetValue {
  /// Wraps [entries]. The map is not copied; treat it as owned by this value.
  const DuetMap(this.entries);

  /// The wrapped key-value entries, in whatever order the caller built them.
  final Map<String, DuetValue> entries;

  /// Emits the keys in the wire's canonical order, **not** insertion order.
  ///
  /// `jsonEncode` walks a Dart `Map` in insertion order, so without this sort
  /// the same logical value encodes to different bytes depending on how it was
  /// built — and never matches Rust's `BTreeMap` output. Sorting at encode
  /// time is what makes the encoding a function of the value alone.
  @override
  Map<String, Object?> toJson() => <String, Object?>{
        't': 'm',
        'v': sortedJsonObject(<String, Object?>{
          for (final MapEntry<String, DuetValue> e in entries.entries)
            e.key: e.value.toJson(),
        }),
      };

  @override
  bool operator ==(Object other) {
    if (other is! DuetMap || other.entries.length != entries.length) {
      return false;
    }
    for (final MapEntry<String, DuetValue> e in entries.entries) {
      final DuetValue? mine = other.entries[e.key];
      if (mine == null || mine != e.value) return false;
    }
    return true;
  }

  @override
  int get hashCode => Object.hashAllUnordered(<Object>[
        for (final MapEntry<String, DuetValue> e in entries.entries)
          Object.hash(e.key, e.value),
      ]);

  @override
  String toString() => 'Map($entries)';
}

/// Decodes an `"i"` payload: a canonical decimal string that fits in an `i64`.
int _decodeInt(Object? payload) {
  if (payload is! String) {
    throw DuetCodecException.badInt('"i" payload must be a decimal string');
  }
  if (!isCanonicalSignedDecimal(payload)) {
    throw DuetCodecException.badInt(
      '"i" payload is not canonical: ${echoBounded(payload)}',
    );
  }
  final int? parsed = int.tryParse(payload);
  if (parsed == null) {
    throw DuetCodecException.badInt(
      '"i" payload overflows i64: ${echoBounded(payload)}',
    );
  }
  return parsed;
}

/// Decodes an `"f"` payload: either a JSON number, or one of the four sentinel
/// strings for the doubles with no portable JSON-number spelling.
///
/// Deliberately wider than [DuetFloat.toJson], mirroring `duet_codec`'s
/// `decode_float`: any JSON number is accepted, so `1` decodes to `1.0` and a
/// literal `-0` still decodes with its sign. A guest hand-building a value has
/// no way to force a decimal point — `JSON.stringify(1.0)` is `"1"` — and
/// should not have to know which spelling this library happens to emit.
/// The canonical quiet NaN, `0x7ff8000000000000` — the bit pattern Rust's
/// `f64::NAN` carries and the one the golden corpus pins for the `NaN`
/// sentinel.
///
/// Built from its bits rather than spelled `double.nan`, because `double.nan`
/// (`0.0 / 0.0`) is materialised with an architecture-dependent sign bit:
/// positive quiet NaN on arm64, **negative** (`0xfff8000000000000`) on x64,
/// where SSE division produces the negative form. The corpus compares floats
/// by their bits — the only comparison that means anything for a NaN — so the
/// decode must produce one spelling on every machine, not whichever NaN the
/// local FPU prefers. Found by the first ubuntu-x64 CI run; every earlier run
/// of this suite was on arm64.
final double _canonicalNan =
    (ByteData(8)..setUint64(0, 0x7ff8000000000000)).getFloat64(0);

double _decodeFloat(Object? payload) {
  if (payload is num) return payload.toDouble();
  if (payload is String) {
    return switch (payload) {
      'NaN' => _canonicalNan,
      'Infinity' => double.infinity,
      '-Infinity' => double.negativeInfinity,
      '-0' => -0.0,
      _ => throw DuetCodecException.badFloat(
          'unrecognised float sentinel ${echoBounded(payload)}',
        ),
    };
  }
  throw DuetCodecException.badFloat(
    '"f" payload must be a number or a sentinel string',
  );
}

/// Decodes a `"b"` payload, converting `dart:convert`'s [FormatException] into
/// this package's own error so nothing from below escapes to the caller.
List<int> _decodeBytes(Object? payload) {
  if (payload is! String) {
    throw DuetCodecException.badBase64('"b" payload must be a string');
  }
  try {
    return base64.decode(payload);
  } on FormatException catch (e) {
    throw DuetCodecException.badBase64('invalid base64: ${e.message}');
  }
}
