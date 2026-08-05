import 'dart:convert';

import 'package:duet/duet.dart';
import 'package:test/test.dart';

void main() {
  group('tagged encoding', () {
    test('matches the duet-codec goldens byte for byte', () {
      // Pins Dart's `toJson()` against crates/duet-codec/src/value.rs's
      // `encode_value` fixtures (`encodes_each_variant_with_its_tag`). If
      // either side's tag or field name drifts, the two guests silently stop
      // agreeing on the wire shape.
      expect(const DuetNull().toWireText(), '{"t":"n"}');
      expect(const DuetBool(true).toWireText(), '{"t":"bool","v":true}');
      expect(const DuetInt(42).toWireText(), '{"t":"i","v":"42"}');
      expect(const DuetFloat(1.5).toWireText(), '{"t":"f","v":1.5}');
      expect(const DuetStr('hi').toWireText(), '{"t":"s","v":"hi"}');
      expect(
          DuetBytes(utf8.encode('foo')).toWireText(), '{"t":"b","v":"Zm9v"}');
      expect(
        const DuetList(<DuetValue>[DuetInt(1)]).toWireText(),
        '{"t":"l","v":[{"t":"i","v":"1"}]}',
      );
      expect(
        const DuetMap(<String, DuetValue>{'k': DuetBool(false)}).toWireText(),
        '{"t":"m","v":{"k":{"t":"bool","v":false}}}',
      );
    });

    test('an Int above 2^53 survives as a decimal string', () {
      // The whole reason Int travels as a string: a JSON *number* above 2^53
      // loses precision once it round-trips through an IEEE-754 double, which
      // is exactly what JSON numbers are in both Dart's `num` and JavaScript.
      // A string sidesteps that entirely, in both directions.
      const int big = 9007199254740993; // 2^53 + 1
      expect(
          const DuetInt(big).toWireText(), '{"t":"i","v":"9007199254740993"}');
      expect(
        DuetValue.fromWireText('{"t":"i","v":"9007199254740993"}'),
        const DuetInt(big),
      );
    });
  });

  group('rejections', () {
    test('a JSON-number Int is rejected, matching Rust', () {
      // Wire rule: Int MUST travel as a decimal string, never a JSON number.
      // `duet-codec/src/value.rs::decode_int` requires `payload.as_str()` and
      // fails immediately if the payload is any other JSON type — a `42` here
      // is not "the same value, less strictly typed", it is a different,
      // rejected shape. If Dart accepted a JSON-number payload, a host that
      // (incorrectly) emitted one would round-trip silently through this
      // client while a Rust guest talking to the same host would reject it —
      // and the two would disagree about what "42" even means once it crosses
      // 2^53, since a JSON number cannot represent every i64 exactly but a
      // decimal string always can.
      expect(
        () => DuetValue.fromWireText('{"t":"i","v":42}'),
        throwsA(
          isA<DuetCodecException>().having(
            (DuetCodecException e) => e.reason,
            'reason',
            DuetReason.badInt,
          ),
        ),
      );
      for (final String bad in <String>[
        '{"t":"i","v":"+5"}',
        '{"t":"i","v":"007"}',
        '{"t":"i","v":"-0"}',
        '{"t":"i","v":"999999999999999999999"}',
        '{"t":"q","v":1}',
        '{"t":"i"}',
        '42',
        '{}',
        '{"t":1}',
        '{"t":"bool","v":"yes"}',
        '{"t":"s","v":1}',
        '{"t":"l","v":{}}',
        '{"t":"m","v":[]}',
        'not json at all',
      ]) {
        expect(
          () => DuetValue.fromWireText(bad),
          throwsA(isA<DuetCodecException>()),
          reason: bad,
        );
      }
    });

    test(
        'a non-base64 Bytes payload throws this package\'s error, not '
        'dart:convert\'s', () {
      // `base64.decode` throws a FormatException, which is neither catchable
      // by `on DuetException` nor distinguishable from a JSON parse failure.
      // Converting it at the boundary is what makes "only DuetException
      // escapes" true.
      for (final String bad in <String>[
        '{"t":"b","v":"!!!"}',
        '{"t":"b","v":"A"}',
        '{"t":"b","v":1}',
      ]) {
        expect(
          () => DuetValue.fromWireText(bad),
          throwsA(
            isA<DuetCodecException>().having(
              (DuetCodecException e) => e.reason,
              'reason',
              DuetReason.badBase64,
            ),
          ),
          reason: bad,
        );
      }
    });

    test('error messages are bounded, whatever the peer sends', () {
      // This decodes untrusted input; an unbounded echo turns a one-megabyte
      // payload into a one-megabyte log line. Mirrors duet-codec's
      // `guest_supplied_text_is_bounded_in_error_messages`.
      final String huge = 'z' * 10000;
      final DuetCodecException e = _codecErrorFrom(
        () => DuetValue.fromWireText('{"t":"$huge","v":1}'),
      );
      expect(e.reason, DuetReason.unknownTag);
      expect(e.message.length, lessThan(300));
      expect(echoBounded(huge).length, maxEchoChars + 3);
    });
  });

  group('nesting', () {
    test('decoding refuses nesting past maxJsonDepth, as serde_json does', () {
      // dart:convert has no recursion limit at all — `jsonDecode` builds a
      // 100 000-deep tree without complaint, iteratively, so it never even
      // overflows the stack to warn anyone. Rust's serde_json refuses past its
      // own limit, which is why the corpus case
      // `value/nesting/exceeds_parser_recursion_limit` has reason "bad_json".
      // Without this guard a Dart guest would accept messages every Rust peer
      // rejects, and would then hand the tree to a recursive decoder.
      String nest(int depth) =>
          '${'{"t":"l","v":[' * depth}{"t":"n"}${']}' * depth}';

      // Two JSON containers per level of value nesting, so the limit bites at
      // half of maxJsonDepth.
      expect(
        DuetValue.fromWireText(nest(maxJsonDepth ~/ 2 - 1)),
        isA<DuetList>(),
      );
      expect(
        () => DuetValue.fromWireText(nest(maxJsonDepth)),
        throwsA(
          isA<DuetCodecException>().having(
            (DuetCodecException e) => e.reason,
            'reason',
            DuetReason.badJson,
          ),
        ),
      );
    });

    test('the guard survives a tree deep enough to overflow a recursive walk',
        () {
      // 100 000 containers: `jsonDecode` builds this, so the depth check has
      // to be iterative or it dies on exactly the input it exists to reject.
      final String deep = '${'[' * 100000}1${']' * 100000}';
      expect(
        () => decodeDuetJson(deep),
        throwsA(
          isA<DuetCodecException>().having(
            (DuetCodecException e) => e.reason,
            'reason',
            DuetReason.badJson,
          ),
        ),
      );
    });
  });

  group('map key order', () {
    test('keys encode in code-point order, not UTF-16 code-unit order', () {
      // The exact mirror of duet-codec/src/value.rs's
      // `map_keys_encode_in_code_point_order_even_across_the_surrogate_range`,
      // asserting the same byte string, and of the corpus case
      // `value/map/code_point_order`.
      //
      // These three keys are chosen so the WRONG order is detectable. U+1F600
      // is non-BMP, so UTF-16 encodes it as the surrogate pair D83D DE00, and
      // 0xD83D < 0xE000. Dart's String.compareTo compares UTF-16 code units,
      // so it sorts U+1F600 FIRST; code-point order — and Rust's BTreeMap —
      // sorts it LAST. A test using ASCII or Latin-1 keys would pass under
      // both rules and prove nothing, which is how this divergence survived.
      const String canonical =
          '{"t":"m","v":{"\u{E000}":{"t":"n"},"\u{FFFD}":{"t":"n"},'
          '"\u{1F600}":{"t":"n"}}}';

      expect(
        const DuetMap(<String, DuetValue>{
          '\u{1F600}': DuetNull(),
          '\u{E000}': DuetNull(),
          '\u{FFFD}': DuetNull(),
        }).toWireText(),
        canonical,
      );

      // Insertion order must not leak into the encoding.
      expect(
        const DuetMap(<String, DuetValue>{
          '\u{FFFD}': DuetNull(),
          '\u{1F600}': DuetNull(),
          '\u{E000}': DuetNull(),
        }).toWireText(),
        canonical,
      );

      // The guard rail: this is the assertion that fails if someone
      // "simplifies" compareDuetMapKeys back to String.compareTo. Pinned
      // explicitly so the reason is visible at the point of failure, not just
      // the symptom.
      expect(
        '\u{1F600}'.compareTo('\u{E000}'),
        lessThan(0),
        reason: 'compareTo puts U+1F600 first — this is the bug being fixed',
      );
      expect(
        compareDuetMapKeys('\u{1F600}', '\u{E000}'),
        greaterThan(0),
        reason: 'code-point order puts U+1F600 last, agreeing with Rust',
      );
    });

    test('compareDuetMapKeys is a total order over awkward keys', () {
      // Prefix handling, the empty string, and equality — the cases a
      // rune-by-rune comparator gets wrong if the exhaustion branches are
      // mixed up. Sorting must agree with the pairwise comparator.
      expect(compareDuetMapKeys('', ''), 0);
      expect(compareDuetMapKeys('', 'a'), lessThan(0));
      expect(compareDuetMapKeys('a', ''), greaterThan(0));
      expect(compareDuetMapKeys('a', 'ab'), lessThan(0));
      expect(compareDuetMapKeys('ab', 'a'), greaterThan(0));
      expect(compareDuetMapKeys('\u{1F600}', '\u{1F600}'), 0);

      final List<String> keys = <String>[
        '\u{1F600}',
        'b',
        '',
        '\u{E000}',
        'a',
        '\u{FFFD}',
        'ab',
      ]..sort(compareDuetMapKeys);
      // Code-point order: '' < 'a' < 'ab' < 'b' < U+E000 < U+FFFD < U+1F600.
      expect(keys, <String>[
        '',
        'a',
        'ab',
        'b',
        '\u{E000}',
        '\u{FFFD}',
        '\u{1F600}',
      ]);
    });

    test('sortedJsonObject reorders envelope fields too', () {
      // The same rule, applied to the envelope rather than to a Value::Map.
      // Rust's serde_json::Map is a BTreeMap, so the host emits
      // {"id",…,"kind",…,"path",…}; Dart's jsonEncode would emit whatever
      // order the literal was written in.
      expect(
        encodeDuetJson(
          sortedJsonObject(<String, Object?>{
            'path': 'a',
            'kind': 'get',
            'id': '1',
          }),
        ),
        '{"id":"1","kind":"get","path":"a"}',
      );
    });
  });

  group('floats', () {
    test('jsonEncode throws on a raw NaN, so the sentinel is mandatory', () {
      // If DuetFloat.toJson() ever "simplified" to emitting `value` directly,
      // this is why it would break: dart:convert's jsonEncode refuses to
      // serialize NaN and the infinities at all, so the string-sentinel
      // conversion is not optional polish — it is the only reason a
      // non-finite float can be sent.
      expect(
        () => jsonEncode(<String, Object?>{'v': double.nan}),
        throwsA(isA<JsonUnsupportedObjectError>()),
      );
      expect(const DuetFloat(double.nan).toWireText(), '{"t":"f","v":"NaN"}');
      expect(
        const DuetFloat(double.infinity).toWireText(),
        '{"t":"f","v":"Infinity"}',
      );
      final DuetValue nan = DuetValue.fromWireText('{"t":"f","v":"NaN"}');
      expect((nan as DuetFloat).value.isNaN, isTrue);
    });

    test('negative zero encodes as a string sentinel', () {
      // Mirrors duet-codec/src/value.rs's
      // `negative_zero_encodes_as_a_string_sentinel`. Dart CAN write -0.0 as a
      // JSON number, but JavaScript cannot — `JSON.stringify(-0)` is "0" — so
      // all three implementations emit the sentinel and the wire has exactly
      // one spelling per value.
      expect(const DuetFloat(-0.0).toWireText(), '{"t":"f","v":"-0"}');
      // Positive zero stays a JSON number: it has a portable spelling.
      expect(const DuetFloat(0.0).toWireText(), '{"t":"f","v":0.0}');
      // The negative-infinity sentinel must not be disturbed by that arm.
      expect(
        const DuetFloat(double.negativeInfinity).toWireText(),
        '{"t":"f","v":"-Infinity"}',
      );
    });

    test('negative zero round-trips with its sign', () {
      // `-0.0 == 0.0` is true in Dart exactly as in IEEE 754, so an equality
      // assertion here would pass even with the sign lost. `isNegative` and
      // `1/x == -infinity` are the two tests that can actually see it.
      for (final String text in <String>[
        '{"t":"f","v":"-0"}',
        '{"t":"f","v":-0.0}',
      ]) {
        final DuetValue decoded = DuetValue.fromWireText(text);
        expect(decoded, isA<DuetFloat>(), reason: text);
        final double f = (decoded as DuetFloat).value;
        expect(f, 0.0, reason: text);
        expect(f.isNegative, isTrue, reason: '$text must keep the sign bit');
        expect(1 / f, double.negativeInfinity, reason: text);
      }
      // The control: positive zero must NOT come back negative, which is the
      // failure mode of testing `value == -0.0` instead of `isNegative`.
      final DuetFloat positive =
          DuetValue.fromWireText('{"t":"f","v":0.0}') as DuetFloat;
      expect(positive.value.isNegative, isFalse);
      expect(1 / positive.value, double.infinity);
    });

    test('the float decoder is wider than the encoder', () {
      // Mirrors duet_codec's `decode_float`: a guest hand-building a value has
      // no way to force a decimal point, and `JSON.stringify(1.0)` is "1", so
      // an integer-looking JSON number must decode as a float.
      expect(
        DuetValue.fromWireText('{"t":"f","v":1}'),
        const DuetFloat(1.0),
      );
      expect(
        DuetValue.fromWireText('{"t":"f","v":0}'),
        const DuetFloat(0.0),
      );
      // An unrecognised sentinel is still rejected — the set is closed at four.
      for (final String bad in <String>[
        '{"t":"f","v":"-0.0"}',
        '{"t":"f","v":"huge"}',
        '{"t":"f","v":true}',
      ]) {
        expect(
          () => DuetValue.fromWireText(bad),
          throwsA(
            isA<DuetCodecException>().having(
              (DuetCodecException e) => e.reason,
              'reason',
              DuetReason.badFloat,
            ),
          ),
          reason: bad,
        );
      }
    });
  });

  group('equality', () {
    test('the container variants compare by content, not identity', () {
      expect(
        const DuetList(<DuetValue>[DuetInt(1), DuetStr('a')]),
        const DuetList(<DuetValue>[DuetInt(1), DuetStr('a')]),
      );
      expect(
        const DuetList(<DuetValue>[DuetInt(1)]),
        isNot(const DuetList(<DuetValue>[DuetInt(2)])),
      );
      expect(DuetBytes(<int>[1, 2]), DuetBytes(<int>[1, 2]));
      expect(DuetBytes(<int>[1, 2]), isNot(DuetBytes(<int>[1, 3])));
      // Insertion order is not part of a map's identity, only of nothing at
      // all: the encoding sorts, so two maps built differently are one value.
      expect(
        const DuetMap(<String, DuetValue>{'a': DuetInt(1), 'b': DuetInt(2)}),
        const DuetMap(<String, DuetValue>{'b': DuetInt(2), 'a': DuetInt(1)}),
      );
      expect(
        const DuetMap(<String, DuetValue>{'a': DuetInt(1)}),
        isNot(const DuetMap(<String, DuetValue>{'a': DuetInt(2)})),
      );
    });

    test('float equality is IEEE-754, which is why the corpus uses bits', () {
      // Documented, not accidental: this mirrors Rust's derived PartialEq on
      // Value::Float(f64). It is also exactly why a corpus written with `==`
      // would be vacuous for NaN and blind to a dropped sign bit.
      expect(
        const DuetFloat(double.nan) == const DuetFloat(double.nan),
        isFalse,
      );
      expect(const DuetFloat(-0.0) == const DuetFloat(0.0), isTrue);
    });
  });

  test('an absent value and a null value stay distinguishable', () {
    // JSON null means "no value at this path"; {"t":"n"} means the path exists
    // and holds null. See crates/duet-protocol/src/wire.rs's `optional_value`.
    expect(DuetValue.optionalFromJson(null), isNull);
    expect(
      DuetValue.optionalFromJson(<String, Object?>{'t': 'n'}),
      const DuetNull(),
    );
  });
}

/// Runs [body] and returns the [DuetCodecException] it threw.
DuetCodecException _codecErrorFrom(void Function() body) {
  try {
    body();
  } on DuetCodecException catch (e) {
    return e;
  }
  fail('expected a DuetCodecException');
}
