/// [duetDecodeOutcome] and its three arms.
///
/// The algorithm behind every generated command method. Generated code is
/// declarations and literals, so this is where the branching is tested — and
/// the arm worth the most attention is the third: a host that answered
/// something the schema's codec cannot read must produce a value the caller can
/// see and log, not an exception and not a silent null.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

/// A codec that decodes exactly one value and refuses everything else.
///
/// Hand-written rather than reused from the generated goldens, so this file
/// tests `duetDecodeOutcome`'s branching rather than a particular codec's.
final class OnlySeven implements DuetCodec<int> {
  const OnlySeven();

  @override
  String get name => 'OnlySeven';

  @override
  DuetValue encode(int value) => DuetInt(value);

  @override
  int? decode(DuetValue value) =>
      value is DuetInt && value.value == 7 ? 7 : null;
}

/// A codec for the error side, so the two are never confusable.
final class OnlyBoom implements DuetCodec<String> {
  const OnlyBoom();

  @override
  String get name => 'OnlyBoom';

  @override
  DuetValue encode(String value) => DuetStr(value);

  @override
  String? decode(DuetValue value) =>
      value is DuetStr && value.value == 'boom' ? 'boom' : null;
}

void main() {
  group('duetDecodeOutcome', () {
    test('a returned value the codec reads becomes DuetOk', () {
      expect(
        duetDecodeOutcome<int, String>(
          const DuetReturned(DuetInt(7)),
          const OnlySeven(),
          const OnlyBoom(),
        ),
        const DuetOk<int, String>(7),
      );
    });

    test('a raised value the codec reads becomes DuetErr', () {
      expect(
        duetDecodeOutcome<int, String>(
          const DuetRaised(DuetStr('boom')),
          const OnlySeven(),
          const OnlyBoom(),
        ),
        const DuetErr<int, String>('boom'),
      );
    });

    test('the return codec is never applied to a raised error', () {
      // The mistake that would pass every happy-path test: a decoder that ran
      // one codec over both arms. `DuetInt(7)` is exactly what the *return*
      // codec accepts, so if it were reached here the answer would be `DuetOk`.
      expect(
        duetDecodeOutcome<int, String>(
          const DuetRaised(DuetInt(7)),
          const OnlySeven(),
          const OnlyBoom(),
        ),
        const DuetUndecodable<int, String>(value: DuetInt(7), raised: true),
      );
    });

    test('the raise codec is never applied to a returned value', () {
      expect(
        duetDecodeOutcome<int, String>(
          const DuetReturned(DuetStr('boom')),
          const OnlySeven(),
          const OnlyBoom(),
        ),
        const DuetUndecodable<int, String>(
          value: DuetStr('boom'),
          raised: false,
        ),
      );
    });

    test('an undecodable answer says which reply it was', () {
      // The flag is the whole reason this arm carries more than a value: a
      // `returned` that did not decode may still have succeeded, and a `raised`
      // that did not decode certainly did not.
      final DuetOutcome<int, String> fromReturn = duetDecodeOutcome<int, String>(
        const DuetReturned(DuetNull()),
        const OnlySeven(),
        const OnlyBoom(),
      );
      final DuetOutcome<int, String> fromRaise = duetDecodeOutcome<int, String>(
        const DuetRaised(DuetNull()),
        const OnlySeven(),
        const OnlyBoom(),
      );
      expect((fromReturn as DuetUndecodable<int, String>).raised, isFalse);
      expect((fromRaise as DuetUndecodable<int, String>).raised, isTrue);
      expect(fromReturn, isNot(equals(fromRaise)),
          reason: 'the two must not be one value');
    });

    test('the dynamic codec makes the undecodable arm unreachable', () {
      // What a command with no declared type is generated with. `dynamic` is
      // the identity, so every answer decodes — which is why a `session.ping`
      // that answers null is a `DuetOk` and not an undecodable.
      for (final DuetValue answered in <DuetValue>[
        const DuetNull(),
        const DuetInt(1),
        const DuetStr('anything'),
        const DuetMap(<String, DuetValue>{}),
      ]) {
        expect(
          duetDecodeOutcome<DuetValue, DuetValue>(
            DuetReturned(answered),
            duetDynamicCodec,
            duetDynamicCodec,
          ),
          DuetOk<DuetValue, DuetValue>(answered),
        );
      }
    });

    test('the arms have value equality and readable descriptions', () {
      expect(const DuetOk<int, String>(7), const DuetOk<int, String>(7));
      expect(const DuetOk<int, String>(7).hashCode,
          const DuetOk<int, String>(7).hashCode);
      expect(const DuetOk<int, String>(7), isNot(const DuetOk<int, String>(8)));
      expect(const DuetOk<int, String>(7).toString(), 'Ok(7)');

      expect(const DuetErr<int, String>('a'), const DuetErr<int, String>('a'));
      expect(const DuetErr<int, String>('a').hashCode,
          const DuetErr<int, String>('a').hashCode);
      expect(
          const DuetErr<int, String>('a'), isNot(const DuetErr<int, String>('b')));
      expect(const DuetErr<int, String>('a').toString(), 'Err(a)');

      const DuetUndecodable<int, String> one =
          DuetUndecodable<int, String>(value: DuetInt(1), raised: false);
      expect(one, const DuetUndecodable<int, String>(value: DuetInt(1), raised: false));
      expect(one.hashCode,
          const DuetUndecodable<int, String>(value: DuetInt(1), raised: false).hashCode);
      expect(one,
          isNot(const DuetUndecodable<int, String>(value: DuetInt(1), raised: true)));
      expect(one.toString(), 'Undecodable(Int(1), raised: false)');
    });

    test('an Ok and an Err with equal payloads are still different outcomes',
        () {
      // A `sealed` hierarchy whose arms compared only their payloads would let
      // a success and a domain failure test equal, which is the one comparison
      // that must never hold.
      expect(const DuetOk<int, int>(1), isNot(const DuetErr<int, int>(1)));
    });
  });
}
