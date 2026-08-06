/// `DuetCodec`, the primitive codecs, and the four readings they produce.
library;

import 'dart:io';

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

import 'editor.dart';

/// A directory the package's own `dart analyze` does not look in, so the
/// deliberately broken sample below cannot fail the real analysis run.
const String _scratch = '.dart_tool/duet_type_bound';

/// Analyses one snippet in isolation and returns what the analyser said.
Future<ProcessResult> _analyse(String name, String source) async {
  final Directory dir = Directory(_scratch)..createSync(recursive: true);
  final File file = File('${dir.path}/$name.dart')..writeAsStringSync(source);
  return Process.run(
    Platform.resolvedExecutable,
    <String>['analyze', file.path],
  );
}

void main() {
  group('primitive codecs round-trip', () {
    test('bool, int, double, String and bytes', () {
      expect(duetBoolCodec.encode(true), const DuetBool(true));
      expect(duetBoolCodec.decode(const DuetBool(true)), isTrue);

      expect(duetIntCodec.encode(7), const DuetInt(7));
      expect(duetIntCodec.decode(const DuetInt(7)), 7);

      expect(duetFloatCodec.encode(1.5), const DuetFloat(1.5));
      expect(duetFloatCodec.decode(const DuetFloat(1.5)), 1.5);

      expect(duetStringCodec.encode('a'), const DuetStr('a'));
      expect(duetStringCodec.decode(const DuetStr('a')), 'a');

      expect(duetBytesCodec.encode(<int>[1, 2]), const DuetBytes(<int>[1, 2]));
      expect(duetBytesCodec.decode(const DuetBytes(<int>[1, 2])), <int>[1, 2]);
    });

    test('every codec refuses a value of another tag', () {
      expect(duetBoolCodec.decode(const DuetInt(1)), isNull);
      expect(duetStringCodec.decode(const DuetBytes(<int>[1])), isNull);
      expect(duetBytesCodec.decode(const DuetStr('a')), isNull);
      expect(duetIntCodec.decode(const DuetNull()), isNull);
    });

    test('the dynamic codec is the identity and refuses nothing', () {
      // The schema's `dynamic` arm says nothing about what is at the path, so
      // nothing at the path can contradict it. A generated `dynamic` field
      // therefore never reports a mismatch — which is the arm's definition
      // rather than a weakening of it.
      for (final DuetValue value in <DuetValue>[
        const DuetNull(),
        const DuetBool(true),
        const DuetInt(7),
        const DuetFloat(1.5),
        const DuetStr('a'),
        const DuetBytes(<int>[1]),
        const DuetList(<DuetValue>[DuetInt(1)]),
        DuetMap(<String, DuetValue>{'a': const DuetInt(1)}),
      ]) {
        expect(duetDynamicCodec.encode(value), value);
        expect(duetDynamicCodec.decode(value), value);
      }
      expect(duetDynamicCodec.name, 'dynamic');
    });

    test('a required dynamic holding a null is present, not none', () {
      // The distinction a `DuetOptionalField` exists to make does not apply
      // here: a `dynamic` field is not an option, so a null someone wrote is
      // the value, not the absence of one.
      expect(
        duetRequiredReading<DuetValue>(duetDynamicCodec, const DuetNull()),
        const DuetPresent<DuetValue>(DuetNull()),
      );
      expect(
        duetRequiredReading<DuetValue>(duetDynamicCodec, null),
        const DuetAbsent<DuetValue>(),
      );
    });

    test('an integer is not silently widened to a double, or the reverse', () {
      // `Value::Int` and `Value::Float` are distinct host types. A codec that
      // accepted one for the other would let a guest write an `i64` where the
      // schema says `f64` and have every other guest agree with it.
      expect(duetFloatCodec.decode(const DuetInt(1)), isNull);
      expect(duetIntCodec.decode(const DuetFloat(1)), isNull);
    });

    test('a list is refused whole if any element is', () {
      final DuetCodec<List<int>> codec = duetListCodec<int>(duetIntCodec);
      expect(codec.name, 'List<int>');
      expect(
        codec.decode(DuetList(<DuetValue>[const DuetInt(1), const DuetInt(2)])),
        <int>[1, 2],
      );
      // Not `[1]`: a partial list is a wrong answer that looks like a right
      // one.
      expect(
        codec.decode(DuetList(<DuetValue>[const DuetInt(1), const DuetStr('x')])),
        isNull,
      );
      expect(codec.decode(const DuetInt(1)), isNull);
    });

    test('a map is refused whole if any value is', () {
      final DuetCodec<Map<String, int>> codec = duetMapCodec<int>(duetIntCodec);
      expect(codec.name, 'Map<String, int>');
      expect(
        codec.decode(DuetMap(<String, DuetValue>{'a': const DuetInt(1)})),
        <String, int>{'a': 1},
      );
      expect(
        codec.decode(DuetMap(<String, DuetValue>{
          'a': const DuetInt(1),
          'b': const DuetStr('x'),
        })),
        isNull,
      );
      expect(codec.decode(const DuetInt(1)), isNull);
    });

    test('a nested codec round-trips a struct', () {
      const Editor editor = Editor(zoom: 2, mode: 'draw');
      final DuetValue encoded = const EditorCodec().encode(editor);
      expect(const EditorCodec().decode(encoded), editor);
      expect(const EditorCodec().decode(const DuetInt(1)), isNull);
    });
  });

  group('readings', () {
    test('a required field: absent, present, mismatch', () {
      expect(
        duetRequiredReading<int>(duetIntCodec, null),
        isA<DuetAbsent<int>>(),
      );
      expect(
        duetRequiredReading<int>(duetIntCodec, const DuetInt(3)),
        const DuetPresent<int>(3),
      );
      // A required field promised a `T`, and a null is not one.
      expect(
        duetRequiredReading<int>(duetIntCodec, const DuetNull()),
        isA<DuetMismatch<int>>(),
      );
      expect(
        duetRequiredReading<int>(duetIntCodec, const DuetStr('x')),
        isA<DuetMismatch<int>>(),
      );
    });

    test('an optional field: absent, none, present, mismatch', () {
      expect(
        duetOptionalReading<int>(duetIntCodec, null),
        isA<DuetAbsent<int>>(),
      );
      expect(
        duetOptionalReading<int>(duetIntCodec, const DuetNull()),
        isA<DuetNone<int>>(),
      );
      expect(
        duetOptionalReading<int>(duetIntCodec, const DuetInt(3)),
        const DuetPresent<int>(3),
      );
      expect(
        duetOptionalReading<int>(duetIntCodec, const DuetStr('x')),
        isA<DuetMismatch<int>>(),
      );
    });

    test('none and absent are not equal to each other', () {
      // The whole distinction in one assertion: if these two ever collapse,
      // `Option<T> = None` and "the struct holding it is itself None" become
      // the same answer.
      expect(const DuetNone<int>(), isNot(const DuetAbsent<int>()));
      expect(const DuetNone<int>().hashCode,
          isNot(const DuetAbsent<int>().hashCode));
    });

    test('a mismatch carries what was actually found', () {
      final DuetReading<int> reading =
          duetRequiredReading<int>(duetIntCodec, const DuetStr('x'));
      expect((reading as DuetMismatch<int>).found, const DuetStr('x'));
      expect(reading.reason, contains('expected int'));
    });

    test('a throwing codec becomes a mismatch, not an escaping exception', () {
      final DuetReading<String> reading =
          duetRequiredReading<String>(const ThrowingCodec(), const DuetInt(1));
      expect(reading, isA<DuetMismatch<String>>());
      expect((reading as DuetMismatch<String>).reason, contains('threw'));
    });

    test('valueOrNull answers only for a present reading', () {
      expect(const DuetPresent<int>(3).valueOrNull, 3);
      expect(const DuetNone<int>().valueOrNull, isNull);
      expect(const DuetAbsent<int>().valueOrNull, isNull);
      expect(
        const DuetMismatch<int>(DuetStr('x'), 'because').valueOrNull,
        isNull,
      );
    });
  });

  group("DuetCodec's non-nullable bound", () {
    // Dart has no in-suite way to assert that a program does *not* compile —
    // no `@ts-expect-error`, and `dart:mirrors` reports the same `Object`
    // upper bound for `<T extends Object>` and for a bare `<T>`, so reflection
    // cannot tell them apart either. The analyser can, so it is asked
    // directly. Both halves matter: without the control below, a harness that
    // always reported an error would "pass" this group while proving nothing.
    setUpAll(() {
      final Directory dir = Directory(_scratch);
      if (dir.existsSync()) dir.deleteSync(recursive: true);
    });

    tearDownAll(() {
      final Directory dir = Directory(_scratch);
      if (dir.existsSync()) dir.deleteSync(recursive: true);
    });

    test('a non-nullable type argument analyses clean (the control)', () async {
      final ProcessResult result = await _analyse('good_codec', '''
import 'package:duet/typed.dart';

abstract class GoodCodec implements DuetCodec<String> {}
''');
      expect(result.exitCode, 0, reason: result.stdout.toString());
    }, timeout: const Timeout(Duration(minutes: 2)));

    test('a nullable type argument is rejected at the definition site',
        () async {
      final ProcessResult result = await _analyse('nullable_codec', '''
import 'package:duet/typed.dart';

abstract class NullableCodec implements DuetCodec<String?> {}
''');
      expect(result.exitCode, isNot(0));
      expect(
        result.stdout.toString(),
        contains('type_argument_not_matching_bounds'),
      );
      expect(result.stdout.toString(), contains("doesn't conform to the bound"));
    }, timeout: const Timeout(Duration(minutes: 2)));
  });
}
