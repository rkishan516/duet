// Mirrors crates/duet-codec/src/value.rs exactly.
import 'dart:convert';

/// Something went wrong turning tagged JSON into a [DuetValue], or a `u64`
/// carried as a decimal string was not in canonical form.
///
/// Thrown, never a bare exception or an assertion: this crate decodes
/// untrusted host input, and an assertion would be stripped in a release
/// build (`--dart-define=dart.vm.product=true` disables `assert`), silently
/// turning a rejection into acceptance.
class DuetCodecException implements Exception {
  DuetCodecException(this.message);

  /// Bounded by [kMaxEchoChars] wherever it embeds guest- or host-supplied
  /// text, mirroring `duet_codec::error::MAX_ECHO_CHARS`.
  final String message;

  @override
  String toString() => 'DuetCodecException: $message';
}

/// Mirrors `duet_core::Value` (crates/duet-core/src/value.rs:13-...).
///
/// A sealed class rather than an enum: each variant carries different data
/// (a `bool`, an `int`, a `List<DuetValue>`, ...), which Dart enums cannot
/// express. `sealed` still gets exhaustiveness checking on `switch`.
sealed class DuetValue {
  const DuetValue();

  /// The tagged JSON form: a `Map<String, Object?>` ready for `jsonEncode`.
  Map<String, Object?> toJson();

  /// Decodes one tagged value.
  ///
  /// Total over all JSON input: every branch either returns a [DuetValue] or
  /// throws [DuetCodecException] — never a bare type-cast failure, which
  /// would surface as an unrelated `TypeError` instead of a diagnosable
  /// message naming the field and the tag.
  static DuetValue fromJson(Object? json) {
    if (json is! Map<String, Object?>) {
      throw DuetCodecException('expected an object, found ${_typeName(json)}');
    }
    final Object? tag = json['t'];
    if (tag == null) throw DuetCodecException('missing "t"');
    if (tag is! String) throw DuetCodecException('"t" must be a string');

    if (tag == 'n') return const DuetNull();

    Object? payload() {
      if (!json.containsKey('v')) {
        throw DuetCodecException('tag "$tag" requires "v"');
      }
      return json['v'];
    }

    switch (tag) {
      case 'bool':
        final Object? v = payload();
        if (v is! bool) {
          throw DuetCodecException('"bool" payload must be a boolean');
        }
        return DuetBool(v);
      case 'i':
        final Object? v = payload();
        if (v is! String) {
          throw DuetCodecException('"i" payload must be a decimal string');
        }
        if (!_isCanonicalSignedDecimal(v)) {
          throw DuetCodecException('"i" payload is not canonical: ${_echo(v)}');
        }
        final int? parsed = int.tryParse(v);
        if (parsed == null) {
          throw DuetCodecException('"i" payload overflows i64: ${_echo(v)}');
        }
        return DuetInt(parsed);
      case 'f':
        return DuetFloat(_decodeFloat(payload()));
      case 's':
        final Object? v = payload();
        if (v is! String) throw DuetCodecException('"s" payload must be a string');
        return DuetStr(v);
      case 'b':
        final Object? v = payload();
        if (v is! String) throw DuetCodecException('"b" payload must be a string');
        try {
          return DuetBytes(base64.decode(v));
        } on FormatException catch (e) {
          throw DuetCodecException('invalid base64: ${e.message}');
        }
      case 'l':
        final Object? v = payload();
        if (v is! List<Object?>) {
          throw DuetCodecException('"l" payload must be an array');
        }
        return DuetList(v.map(DuetValue.fromJson).toList(growable: false));
      case 'm':
        final Object? v = payload();
        if (v is! Map<String, Object?>) {
          throw DuetCodecException('"m" payload must be an object');
        }
        return DuetMap(<String, DuetValue>{
          for (final MapEntry<String, Object?> e in v.entries)
            e.key: DuetValue.fromJson(e.value),
        });
      default:
        throw DuetCodecException('unknown type tag ${_echo(tag)}');
    }
  }

  /// JSON `null` means "no value at this path" — distinct from `{"t":"n"}`,
  /// which is `Value::Null`. See duet-protocol/src/wire.rs:159-164.
  static DuetValue? optionalFromJson(Object? json) =>
      json == null ? null : DuetValue.fromJson(json);
}

/// `Value::Null` — the absence of a value, distinct from a path that does not
/// exist at all (see [DuetValue.optionalFromJson]).
final class DuetNull extends DuetValue {
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

/// `Value::Bool`.
final class DuetBool extends DuetValue {
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

/// `Value::Int` — a signed 64-bit integer.
final class DuetInt extends DuetValue {
  const DuetInt(this.value);

  /// The wrapped signed 64-bit integer.
  final int value;

  /// A decimal STRING, never a JSON number: duet-codec/src/value.rs:32.
  /// `int.toString()` is already canonical (no `+`, no leading zeros, no `-0`),
  /// which is what duet-codec/src/canonical.rs:22-27 requires.
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

/// `Value::Float` — a 64-bit float, including NaN and both infinities.
final class DuetFloat extends DuetValue {
  const DuetFloat(this.value);

  /// The wrapped double, which may be NaN or either infinity.
  final double value;

  /// The four doubles with no portable JSON-number spelling travel as string
  /// sentinels: duet-codec/src/value.rs `encode_float`.
  ///
  /// `jsonEncode` THROWS on NaN/Infinity, so those three are mandatory here,
  /// not cosmetic. `-0.0` is different: Dart CAN write it as a JSON number
  /// and `jsonEncode` preserves the sign, but JavaScript cannot
  /// (`JSON.stringify(-0)` is `"0"`), so the sentinel exists for the JS guest
  /// and all three implementations emit it for consistency — the golden
  /// corpus must have one spelling per value, not one per language.
  ///
  /// `value.isNegative` is the test, not `value == -0.0`: `-0.0 == 0.0` is
  /// true in Dart, so an equality check would tag every zero as negative.
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
  @override
  bool operator ==(Object other) => other is DuetFloat && other.value == value;
  @override
  int get hashCode => value.hashCode;
  @override
  String toString() => 'Float($value)';
}

/// `Value::Str`.
final class DuetStr extends DuetValue {
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

/// `Value::Bytes`. Kept distinguishable from [DuetStr] by its own tag, even
/// though both encode to a JSON string — the single clearest reason the
/// encoding is tagged at all.
final class DuetBytes extends DuetValue {
  const DuetBytes(this.value);

  /// The wrapped raw bytes.
  final List<int> value;
  @override
  Map<String, Object?> toJson() =>
      <String, Object?>{'t': 'b', 'v': base64.encode(value)};
  @override
  String toString() => 'Bytes(${value.length} bytes)';
}

/// `Value::List`.
final class DuetList extends DuetValue {
  const DuetList(this.items);

  /// The wrapped, order-preserving list of values.
  final List<DuetValue> items;
  @override
  Map<String, Object?> toJson() => <String, Object?>{
    't': 'l',
    'v': items.map((DuetValue v) => v.toJson()).toList(growable: false),
  };
  @override
  String toString() => 'List($items)';
}

/// `Value::Map`.
final class DuetMap extends DuetValue {
  const DuetMap(this.entries);

  /// The wrapped key-value entries.
  final Map<String, DuetValue> entries;
  @override
  Map<String, Object?> toJson() => <String, Object?>{
    't': 'm',
    'v': <String, Object?>{
      for (final MapEntry<String, DuetValue> e in entries.entries)
        e.key: e.value.toJson(),
    },
  };
  @override
  String toString() => 'Map($entries)';
}

/// Decodes an `"f"` payload: either a JSON number, or one of the four
/// sentinel strings `duet-codec` emits for the doubles with no portable
/// JSON-number spelling (NaN, both infinities, and `-0.0`).
///
/// Deliberately wider than [DuetFloat.toJson], mirroring
/// `duet_codec`'s `decode_float`: any JSON number is accepted, so `1` decodes
/// to `1.0` and a literal `-0` still decodes with its sign. A guest that
/// hand-builds a value should not have to know which spelling this library
/// happens to emit.
double _decodeFloat(Object? payload) {
  if (payload is num) return payload.toDouble();
  if (payload is String) {
    switch (payload) {
      case 'NaN':
        return double.nan;
      case 'Infinity':
        return double.infinity;
      case '-Infinity':
        return double.negativeInfinity;
      case '-0':
        return -0.0;
      default:
        throw DuetCodecException('unrecognised float sentinel ${_echo(payload)}');
    }
  }
  throw DuetCodecException('"f" payload must be a number or a sentinel string');
}

/// Mirrors duet-codec/src/canonical.rs:13-27.
///
/// Exposed (not `_`-prefixed) because [DuetClient]'s wire-level id fields
/// need the same rule and this is the one place it should be defined.
bool isCanonicalUnsignedDigits(String s) => _isCanonicalUnsignedDigits(s);

bool _isCanonicalUnsignedDigits(String s) {
  if (s.isEmpty) return false;
  for (final int c in s.codeUnits) {
    if (c < 0x30 || c > 0x39) return false;
  }
  return s == '0' || !s.startsWith('0');
}

bool _isCanonicalSignedDecimal(String s) {
  if (s.startsWith('-')) {
    final String magnitude = s.substring(1);
    return magnitude != '0' && _isCanonicalUnsignedDigits(magnitude);
  }
  return _isCanonicalUnsignedDigits(s);
}

String _typeName(Object? json) => switch (json) {
  null => 'null',
  bool _ => 'a boolean',
  num _ => 'a number',
  String _ => 'a string',
  List<Object?> _ => 'an array',
  _ => 'an object',
};

/// Mirrors duet_codec::error::MAX_ECHO_CHARS = 48: never echo unbounded
/// host- or guest-supplied text into an error string.
const int kMaxEchoChars = 48;
String _echo(String s) =>
    s.length <= kMaxEchoChars ? '"$s"' : '"${s.substring(0, kMaxEchoChars)}…"';
