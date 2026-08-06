/// `Option<T>` end to end: the three different things the host does at a child
/// of a `None` struct, and the difference between `None` and "no such path".
///
/// Written before the typed interface was frozen, against behaviour measured
/// on the real host:
///
/// ```text
/// with editor: Option<Editor> = None
///   get       at editor.zoom -> null
///   set       at editor.zoom -> FAILED, "wrong kind of node"
///   subscribe at editor.zoom -> ok
/// ```
///
/// The typed layer must not paper over any of the three.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

import 'editor.dart';
import 'fake_host.dart';

/// The seed: an `Option<Editor>` and an `Option<i64>` both `None`, plus one
/// ordinary field so the tests can tell "absent" from "the store is empty".
DuetValue _seed() => DuetMap(<String, DuetValue>{
      'editor': const DuetNull(),
      'count': const DuetNull(),
      'title': const DuetStr('untitled'),
    });

void main() {
  group('the fake host matches what the real host was measured to do', () {
    // These three assertions are what make every other test in this file mean
    // something. If the fake ever drifts from the host, this group fails here
    // rather than silently blessing a typed layer built on a wrong assumption.
    test('get at a child of a None struct answers null', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetClient client = DuetClient(host);

      expect(await client.get('editor.zoom'), isNull);
    });

    test('set at a child of a None struct is refused as the wrong kind of node',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetClient client = DuetClient(host);

      await expectLater(
        client.set('editor.zoom', const DuetFloat(2)),
        throwsA(
          isA<DuetFailure>().having(
            (DuetFailure e) => e.message,
            'message',
            'path "editor.zoom" addresses the wrong kind of node',
          ),
        ),
      );
    });

    test('subscribe at a child of a None struct succeeds', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetClient client = DuetClient(host);

      final DuetSubscription subscription = await client.subscribe('editor.zoom');
      expect(subscription.snapshot, isNull);
      expect(host.subscriptions, hasLength(1));
    });
  });

  group('DuetOptionalField keeps None and absent apart', () {
    test('a path holding Value::Null reads as None, not absent', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetOptionalField<Editor> editor =
          DuetOptionalField<Editor>(router, 'editor', const EditorCodec());

      expect(await editor.get(), isA<DuetNone<Editor>>());
    });

    test('a path that does not exist reads as absent, not None', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetOptionalField<Editor> missing =
          DuetOptionalField<Editor>(router, 'nowhere', const EditorCodec());

      expect(await missing.get(), isA<DuetAbsent<Editor>>());
    });

    test('an Option<i64> distinguishes None from absent as well', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetOptionalField<int> count =
          DuetOptionalField<int>(router, 'count', duetIntCodec);
      final DuetOptionalField<int> missing =
          DuetOptionalField<int>(router, 'tally', duetIntCodec);

      expect(await count.get(), isA<DuetNone<int>>());
      expect(await missing.get(), isA<DuetAbsent<int>>());
    });

    test('writing null writes Value::Null, which reads back as None', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetOptionalField<int> count =
          DuetOptionalField<int>(router, 'count', duetIntCodec);

      await count.set(7);
      expect(await count.get(), const DuetPresent<int>(7));
      expect(host.valueAt('count'), const DuetInt(7));

      await count.set(null);
      expect(await count.get(), isA<DuetNone<int>>());
      // Not "the key vanished": the wire has no delete, and `None` is a value.
      expect(host.valueAt('count'), const DuetNull());
    });

    test('a wrong-typed value is a mismatch, not an exception', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetOptionalField<int> count =
          DuetOptionalField<int>(router, 'count', duetIntCodec);

      // Another guest writes a string where the schema says an integer.
      expect(host.write('count', const DuetStr('lots')), isNull);

      final DuetReading<int> reading = await count.get();
      expect(reading, isA<DuetMismatch<int>>());
      expect((reading as DuetMismatch<int>).found, const DuetStr('lots'));
      expect(reading.reason, contains('expected int'));
    });
  });

  group('a required field under a None struct', () {
    test('reads as absent, and never as None', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetField<double> zoom =
          DuetField<double>(router, 'editor.zoom', duetFloatCodec);

      final DuetReading<double> reading = await zoom.get();
      expect(reading, isA<DuetAbsent<double>>());
      expect(reading, isNot(isA<DuetNone<double>>()));
    });

    test('a write is refused, and says so rather than no-opping', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetField<double> zoom =
          DuetField<double>(router, 'editor.zoom', duetFloatCodec);

      await expectLater(
        zoom.set(2),
        throwsA(
          isA<DuetFailure>().having(
            (DuetFailure e) => e.message,
            'message',
            contains('wrong kind of node'),
          ),
        ),
      );
      // And the store really is untouched, so the failure was not cosmetic.
      expect(host.valueAt('editor'), const DuetNull());
    });

    test('a watch still succeeds, and reports absent', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetField<double> zoom =
          DuetField<double>(router, 'editor.zoom', duetFloatCodec);

      final DuetWatch<double> watch = await zoom.watch((_) {});
      expect(watch.current, isA<DuetAbsent<double>>());
      expect(host.subscriptions, hasLength(1));
    });

    test('a required field holding Value::Null is a mismatch, not None',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      // `title` exists and is a string; another guest nulls it out.
      expect(host.write('title', const DuetNull()), isNull);
      final DuetField<String> title =
          DuetField<String>(router, 'title', duetStringCodec);

      final DuetReading<String> reading = await title.get();
      expect(reading, isA<DuetMismatch<String>>());
      expect((reading as DuetMismatch<String>).found, const DuetNull());
    });
  });

  group('the None/Some transition survives a watch', () {
    test('a child watcher sees absent, then the value, then absent again',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetField<double> zoom =
          DuetField<double>(router, 'editor.zoom', duetFloatCodec);

      final List<DuetReading<double>> seen = <DuetReading<double>>[];
      final DuetWatch<double> watch = await zoom.watch(seen.add);
      expect(watch.current, isA<DuetAbsent<double>>());

      // Some other guest gives `editor` a value: an ancestor write.
      expect(
        host.write(
          'editor',
          const EditorCodec().encode(const Editor(zoom: 2, mode: 'draw')),
        ),
        isNull,
      );
      await router.settled();
      expect(watch.current, const DuetPresent<double>(2));

      // ...then a write to the leaf itself.
      expect(host.write('editor.zoom', const DuetFloat(3)), isNull);
      await router.settled();
      expect(watch.current, const DuetPresent<double>(3));

      // ...then back to None, which makes the child absent again.
      expect(host.write('editor', const DuetNull()), isNull);
      await router.settled();
      expect(watch.current, isA<DuetAbsent<double>>());

      expect(
        seen,
        <DuetReading<double>>[
          const DuetPresent<double>(2),
          const DuetPresent<double>(3),
          const DuetAbsent<double>(),
        ],
      );
    });

    test('an optional field watcher reports None and Some separately',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetOptionalField<Editor> editor =
          DuetOptionalField<Editor>(router, 'editor', const EditorCodec());

      final List<DuetReading<Editor>> seen = <DuetReading<Editor>>[];
      final DuetWatch<Editor> watch = await editor.watch(seen.add);
      expect(watch.current, isA<DuetNone<Editor>>());

      const Editor value = Editor(zoom: 1.5, mode: 'select');
      expect(host.write('editor', const EditorCodec().encode(value)), isNull);
      await router.settled();

      expect(seen, <DuetReading<Editor>>[const DuetPresent<Editor>(value)]);
      expect(watch.current, const DuetPresent<Editor>(value));
    });
  });
}
