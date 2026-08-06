/// The generated client, driven against the **real Rust host**.
///
/// # The gap this closes
///
/// `generated_client_test.dart` drives the same generated code against
/// `test/typed/fake_host.dart` — a Dart stand-in transcribed by hand from
/// `crates/duet-core/src/value.rs`, refusal messages included. That covers the
/// codecs and the wire text. It cannot cover the one thing a transcription can
/// get wrong, which is the transcription: a fake that refuses a write the real
/// host accepts, or accepts one it refuses, passes its own tests forever.
///
/// So this file spawns `crates/duet-host-stdio`, which wraps
/// `duet_protocol::handle_text` in a process speaking newline-delimited JSON,
/// and drives the **committed goldens** against it over a real pipe. What a
/// user gets is what runs here.
///
/// # Every assertion pins an exact value
///
/// A conformance run that spawned a host and asserted "no error" would pass
/// with a completely broken type mapping. Every check below names the value it
/// expects — `3.25`, not "a double"; `{"t":"i","v":"7"}`, not "some JSON" —
/// and the two directions are pinned against two different producers:
///
/// - a **typed write** is read back raw at the schema's own wire path and
///   compared against the wire text Rust put in `corpus/schema-corpus.json`;
/// - a **raw write** of that same Rust-produced text is read back through the
///   typed accessor and compared against a Dart value written by hand here.
///
/// A wrong path literal fails the first. A wrong codec fails the second. A
/// camel-cased wire key fails both.
///
/// # Coverage is asserted, not hoped for
///
/// The table of accessors below is hand-written, and
/// `every path in the corpus has an accessor` asserts its keys are exactly the
/// paths `corpus/schema-corpus.json` states. A schema field added without a
/// case here fails; a case whose path no longer exists fails.
///
/// # Skipping
///
/// Without a `duet-host-stdio` binary these tests skip, loudly, naming the
/// build command. With `DUET_HOST_STDIO` set they may **not** skip: see
/// `support/live_host.dart`.
library;

import 'dart:async';

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

// Prefixed because both goldens declare an `Editor`.
import 'generated/app.duet.dart' as app;
import 'generated/wide.duet.dart' as wide;
import 'support/live_host.dart';
import 'support/schema_corpus.dart';

/// One generated accessor, and the exact value it must carry both ways.
final class Accessor {
  /// Describes the accessor bound at [path].
  const Accessor(
    this.path, {
    required this.write,
    required this.read,
    required this.check,
  });

  /// The wire path the corpus states for this accessor.
  final String path;

  /// Writes the value the corpus admits, through the typed accessor.
  final Future<void> Function() write;

  /// Reads through the typed accessor.
  final Future<DuetReading<Object>> Function() read;

  /// Asserts [read]'s answer is exactly the expected typed value.
  final void Function(DuetReading<Object> reading) check;
}

/// Asserts [reading] is present and holds exactly [value].
///
/// `equals` rather than `==`: Dart's `List` and `Map` compare by identity, so a
/// `DuetPresent<List<String>>` never equals another one however alike they are.
/// The generated data classes inherit that — see [_checkWide] for the one type
/// where it bites.
void _present<T extends Object>(DuetReading<Object> reading, T value) {
  expect(
    reading,
    isA<DuetPresent<T>>(),
    reason: 'expected a present $T, got $reading',
  );
  expect((reading as DuetPresent<T>).value, equals(value));
}

/// The `Editor` every accepted value in the corpus carries.
const app.Editor _appEditor = app.Editor(zoom: 3.25, theme: 'sample');
const wide.Editor _wideEditor = wide.Editor(zoom: 3.25, theme: 'sample');

/// The `App` the corpus admits at the root.
const app.App _app = app.App(
  counter: 7,
  editor: _appEditor,
  title: 'sample',
);

/// The `Wide` the corpus admits at the root.
final wide.Wide _wide = wide.Wide(
  flag: true,
  count: 7,
  ratio: 3.25,
  label: 'sample',
  blob: const <int>[1, 2, 3],
  anything: const DuetStr('anything'),
  maybeLabel: 'sample',
  maybeRatios: const <double>[3.25],
  maybeEditor: _wideEditor,
  tags: const <String>['sample'],
  matrix: const <List<int>>[
    <int>[7]
  ],
  lookup: const <String, int>{'k': 7},
  editors: const <String, wide.Editor>{'k': _wideEditor},
  blobs: const <List<int>>[
    <int>[1, 2, 3]
  ],
  loose: const <String, DuetValue>{'k': DuetStr('anything')},
  flags: const <bool>[true],
  outer: const wide.Outer(inner: _wideEditor, depth: 7),
  snakeCaseField: 'sample',
);

/// Asserts a reading holds exactly [_wide], field by field.
///
/// `Wide`'s generated `operator ==` compares its `List` and `Map` members with
/// `==`, which is identity in Dart, so two structurally identical `Wide`s are
/// never equal. That is a property of the generated code and not a defect —
/// value equality for collections is not something a data class can give — but
/// it means the root accessor needs its fields compared one at a time.
void _checkWide(DuetReading<Object> reading) {
  expect(reading, isA<DuetPresent<wide.Wide>>(), reason: 'got $reading');
  final wide.Wide found = (reading as DuetPresent<wide.Wide>).value;
  expect(found.flag, isTrue);
  expect(found.count, 7);
  expect(found.ratio, 3.25);
  expect(found.label, 'sample');
  expect(found.blob, equals(<int>[1, 2, 3]));
  expect(found.anything, const DuetStr('anything'));
  expect(found.maybeLabel, 'sample');
  expect(found.maybeRatios, equals(<double>[3.25]));
  expect(found.maybeEditor, _wideEditor);
  expect(found.tags, equals(<String>['sample']));
  expect(
      found.matrix,
      equals(<List<int>>[
        <int>[7]
      ]));
  expect(found.lookup, equals(<String, int>{'k': 7}));
  expect(found.editors, equals(<String, wide.Editor>{'k': _wideEditor}));
  expect(
      found.blobs,
      equals(<List<int>>[
        <int>[1, 2, 3]
      ]));
  expect(found.loose, equals(<String, DuetValue>{'k': DuetStr('anything')}));
  expect(found.flags, equals(<bool>[true]));
  expect(found.outer, const wide.Outer(inner: _wideEditor, depth: 7));
  expect(found.snakeCaseField, 'sample');
}

/// Every accessor `app.duet.dart` generates.
///
/// Takes a *supplier* rather than a client, because the table is built when the
/// group is declared and the router only exists once the host has started. Each
/// closure resolves the client at the moment it runs.
List<Accessor> _appAccessors(DuetRouter Function() router) {
  app.AppClient c() => app.AppClient(router());
  return <Accessor>[
    Accessor('',
        write: () => c().self.set(_app),
        read: () => c().self.get(),
        check: (DuetReading<Object> r) => _present<app.App>(r, _app)),
    Accessor('counter',
        write: () => c().counter.set(7),
        read: () => c().counter.get(),
        check: (DuetReading<Object> r) => _present<int>(r, 7)),
    Accessor('editor',
        write: () => c().editor.self.set(_appEditor),
        read: () => c().editor.self.get(),
        check: (DuetReading<Object> r) => _present<app.Editor>(r, _appEditor)),
    Accessor('editor.theme',
        write: () => c().editor.theme.set('sample'),
        read: () => c().editor.theme.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
    Accessor('editor.zoom',
        write: () => c().editor.zoom.set(3.25),
        read: () => c().editor.zoom.get(),
        check: (DuetReading<Object> r) => _present<double>(r, 3.25)),
    Accessor('title',
        write: () => c().title.set('sample'),
        read: () => c().title.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
  ];
}

/// Every accessor `wide.duet.dart` generates. See [_appAccessors] for why this
/// takes a supplier.
List<Accessor> _wideAccessors(DuetRouter Function() router) {
  wide.WideClient c() => wide.WideClient(router());
  return <Accessor>[
    Accessor('',
        write: () => c().self.set(_wide),
        read: () => c().self.get(),
        check: _checkWide),
    Accessor('anything',
        write: () => c().anything.set(const DuetStr('anything')),
        read: () => c().anything.get(),
        check: (DuetReading<Object> r) =>
            _present<DuetValue>(r, const DuetStr('anything'))),
    Accessor('blob',
        write: () => c().blob.set(const <int>[1, 2, 3]),
        read: () => c().blob.get(),
        check: (DuetReading<Object> r) =>
            _present<List<int>>(r, const <int>[1, 2, 3])),
    Accessor('blobs',
        write: () => c().blobs.set(const <List<int>>[
              <int>[1, 2, 3]
            ]),
        read: () => c().blobs.get(),
        check: (DuetReading<Object> r) =>
            _present<List<List<int>>>(r, const <List<int>>[
              <int>[1, 2, 3]
            ])),
    Accessor('count',
        write: () => c().count.set(7),
        read: () => c().count.get(),
        check: (DuetReading<Object> r) => _present<int>(r, 7)),
    Accessor('editors',
        write: () =>
            c().editors.set(const <String, wide.Editor>{'k': _wideEditor}),
        read: () => c().editors.get(),
        check: (DuetReading<Object> r) => _present<Map<String, wide.Editor>>(
            r, const <String, wide.Editor>{'k': _wideEditor})),
    Accessor('flag',
        write: () => c().flag.set(true),
        read: () => c().flag.get(),
        check: (DuetReading<Object> r) => _present<bool>(r, true)),
    Accessor('flags',
        write: () => c().flags.set(const <bool>[true]),
        read: () => c().flags.get(),
        check: (DuetReading<Object> r) =>
            _present<List<bool>>(r, const <bool>[true])),
    Accessor('label',
        write: () => c().label.set('sample'),
        read: () => c().label.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
    Accessor('lookup',
        write: () => c().lookup.set(const <String, int>{'k': 7}),
        read: () => c().lookup.get(),
        check: (DuetReading<Object> r) =>
            _present<Map<String, int>>(r, const <String, int>{'k': 7})),
    Accessor('loose',
        write: () =>
            c().loose.set(const <String, DuetValue>{'k': DuetStr('anything')}),
        read: () => c().loose.get(),
        check: (DuetReading<Object> r) => _present<Map<String, DuetValue>>(
            r, const <String, DuetValue>{'k': DuetStr('anything')})),
    Accessor('matrix',
        write: () => c().matrix.set(const <List<int>>[
              <int>[7]
            ]),
        read: () => c().matrix.get(),
        check: (DuetReading<Object> r) =>
            _present<List<List<int>>>(r, const <List<int>>[
              <int>[7]
            ])),
    Accessor('maybe_editor',
        write: () => c().maybeEditor.self.set(_wideEditor),
        read: () => c().maybeEditor.self.get(),
        check: (DuetReading<Object> r) =>
            _present<wide.Editor>(r, _wideEditor)),
    Accessor('maybe_editor.theme',
        write: () => c().maybeEditor.theme.set('sample'),
        read: () => c().maybeEditor.theme.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
    Accessor('maybe_editor.zoom',
        write: () => c().maybeEditor.zoom.set(3.25),
        read: () => c().maybeEditor.zoom.get(),
        check: (DuetReading<Object> r) => _present<double>(r, 3.25)),
    Accessor('maybe_label',
        write: () => c().maybeLabel.set('sample'),
        read: () => c().maybeLabel.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
    Accessor('maybe_ratios',
        write: () => c().maybeRatios.set(const <double>[3.25]),
        read: () => c().maybeRatios.get(),
        check: (DuetReading<Object> r) =>
            _present<List<double>>(r, const <double>[3.25])),
    Accessor('outer',
        write: () =>
            c().outer.self.set(const wide.Outer(inner: _wideEditor, depth: 7)),
        read: () => c().outer.self.get(),
        check: (DuetReading<Object> r) => _present<wide.Outer>(
            r, const wide.Outer(inner: _wideEditor, depth: 7))),
    Accessor('outer.depth',
        write: () => c().outer.depth.set(7),
        read: () => c().outer.depth.get(),
        check: (DuetReading<Object> r) => _present<int>(r, 7)),
    Accessor('outer.inner',
        write: () => c().outer.inner.self.set(_wideEditor),
        read: () => c().outer.inner.self.get(),
        check: (DuetReading<Object> r) =>
            _present<wide.Editor>(r, _wideEditor)),
    Accessor('outer.inner.theme',
        write: () => c().outer.inner.theme.set('sample'),
        read: () => c().outer.inner.theme.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
    Accessor('outer.inner.zoom',
        write: () => c().outer.inner.zoom.set(3.25),
        read: () => c().outer.inner.zoom.get(),
        check: (DuetReading<Object> r) => _present<double>(r, 3.25)),
    Accessor('ratio',
        write: () => c().ratio.set(3.25),
        read: () => c().ratio.get(),
        check: (DuetReading<Object> r) => _present<double>(r, 3.25)),
    Accessor('snake_case_field',
        write: () => c().snakeCaseField.set('sample'),
        read: () => c().snakeCaseField.get(),
        check: (DuetReading<Object> r) => _present<String>(r, 'sample')),
    Accessor('tags',
        write: () => c().tags.set(const <String>['sample']),
        read: () => c().tags.get(),
        check: (DuetReading<Object> r) =>
            _present<List<String>>(r, const <String>['sample'])),
  ];
}

void main() {
  final String? skip = liveHostSkipReason();
  final SchemaCorpus corpus = SchemaCorpus.load();

  _conformance('app', corpus['app'], _appAccessors, skip: skip);
  _conformance('wide', corpus['wide'], _wideAccessors, skip: skip);

  _optionBehaviours(corpus['wide'], skip: skip);
  _subscriptions(corpus['app'], skip: skip);
}

/// The accessor sweep for one schema.
void _conformance(
  String name,
  CorpusSchema schema,
  List<Accessor> Function(DuetRouter Function() router) build, {
  required String? skip,
}) {
  group('$name, against the real host', () {
    late StdioHost host;
    late DuetClient client;
    late DuetRouter router;
    // Built now, at declaration time, because the case list below is what
    // `test()` calls are generated from. Every closure in it resolves `router`
    // when it runs, by which time `setUpAll` has assigned it.
    final List<Accessor> accessors = build(() => router);

    setUpAll(() async {
      host = await StdioHost.start(name);
      client = DuetClient(host);
      router = DuetRouter(client)..attach();
    });

    tearDownAll(() async {
      await host.close();
    });

    // Every case starts from the seed, so no case can be made to pass by an
    // earlier one's write.
    setUp(() async {
      await client.set('', schema.seed);
    });

    test('every path in the corpus has an accessor, and the reverse', () {
      // The coverage assertion. Without it the sweep below tests whatever
      // happens to be in the table, and a field added to the schema would be
      // generated, committed and never driven against a host.
      expect(
        accessors.map((Accessor a) => a.path).toSet(),
        schema.paths.map((CorpusPath p) => p.path).toSet(),
      );
    });

    test('the store starts at the seed the corpus states', () async {
      // Read raw at the root: the value the whole sweep's "reset" depends on,
      // and the value every claim about an unwritten path is made against.
      final StdioHost fresh = await StdioHost.start(name);
      try {
        final DuetClient reader = DuetClient(fresh);
        expect(await reader.get(''), schema.seed);
      } finally {
        await fresh.close();
      }
    });

    for (final Accessor accessor in accessors) {
      final CorpusPath expected = schema.at(accessor.path);

      test('"${accessor.path}" writes where the schema says', () async {
        // Typed write, then a **raw** read at the schema's own wire path,
        // compared against the wire text Rust generated. Reading back through
        // the same accessor would agree with itself whatever path it used.
        await _materialize(client, schema, accessor.path);
        await accessor.write();
        expect(
          await client.get(accessor.path),
          expected.accept,
          reason: 'the accessor for "${accessor.path}" wrote somewhere else, '
              'or encoded a ${expected.ty} differently than the host does',
        );
      });

      test('"${accessor.path}" reads what another guest wrote', () async {
        // The cross-guest direction: a raw write at the wire path, exactly as
        // a second guest sharing this store would make it, read back through
        // the typed accessor. A camel-cased path literal reads nothing here.
        await _materialize(client, schema, accessor.path);
        await client.set(accessor.path, expected.accept);
        accessor.check(await accessor.read());
      });

      if (expected.rejects.isNotEmpty) {
        test('"${accessor.path}" reports a foreign type as a mismatch',
            () async {
          // Two guests share one store, so a typed accessor will meet a value
          // its codec refuses. That must be reported, not thrown, and not
          // silently widened into a wrong value.
          for (final DuetValue reject in expected.rejects) {
            await client.set('', schema.seed);
            await _materialize(client, schema, accessor.path);
            await client.set(accessor.path, reject);

            final DuetReading<Object> reading = await accessor.read();
            expect(
              reading,
              isA<DuetMismatch<Object>>(),
              reason: '"${accessor.path}" is a ${expected.ty} and read '
                  '${reject.toWireText()} as $reading',
            );
            expect((reading as DuetMismatch<Object>).found, reject);
          }
        });
      }
    }

    tearDownAll(() {
      expect(
        host.unmatched,
        isEmpty,
        reason: 'the host sent lines answering no request; a transport that '
            'mis-correlates would hand this client another call\'s answer',
      );
    });
  }, skip: skip);
}

/// The three measured `Option<Struct>` behaviours, against the real host.
void _optionBehaviours(CorpusSchema schema, {required String? skip}) {
  group('an Option<Struct> that is None, against the real host', () {
    late StdioHost host;
    late DuetClient client;
    late DuetRouter router;
    wide.WideClient c() => wide.WideClient(router);

    setUpAll(() async {
      host = await StdioHost.start('wide');
      client = DuetClient(host);
      router = DuetRouter(client)..attach();
    });

    tearDownAll(() async {
      await host.close();
    });

    setUp(() async {
      await client.set('', schema.seed);
    });

    test('the seed really does leave it None, and its children absent', () {
      // The premise. Without it every assertion below would be about a state
      // the host was never in.
      expect(schema.at('maybe_editor').seed, const DuetNull());
      expect(schema.at('maybe_editor.zoom').seed, isNull);
    });

    test('the option itself reads None, and never absent', () async {
      expect(await c().maybeEditor.self.get(), isA<DuetNone<wide.Editor>>());
    });

    test('a child get answers absent, and never None', () async {
      // Measured on the real host: `get` at a child of a `None` struct answers
      // null, which this layer reports as absent. `None` would be a lie — the
      // path holds nothing at all.
      final DuetReading<double> reading = await c().maybeEditor.zoom.get();
      expect(reading, isA<DuetAbsent<double>>());
      expect(reading, isNot(isA<DuetNone<double>>()));
    });

    test('a child set fails, carrying the host\'s own refusal', () async {
      // THE assertion this whole increment exists for. `fake_host.dart`
      // transcribes this message from `crates/duet-core/src/value.rs`; here it
      // comes from the host itself, so a transcription that had drifted fails.
      await expectLater(
        c().maybeEditor.zoom.set(2),
        throwsA(
          isA<DuetFailure>().having(
            (DuetFailure e) => e.message,
            'message',
            // The whole string a guest sees, prefix included. The prefix is
            // `duet_runtime::RuntimeError::Store`'s `Display`, and until this
            // run existed `test/typed/fake_host.dart` omitted it — so every
            // exact-message assertion in this package was written against a
            // string the real host never sends.
            'store rejected the write: path "maybe_editor.zoom" addresses '
                'the wrong kind of node',
          ),
        ),
      );
      // ...and the store really is untouched, so the refusal was not cosmetic.
      expect(await client.get('maybe_editor'), const DuetNull());
    });

    test('a child subscribe succeeds, and reports absent', () async {
      // The third behaviour, and the one that differs from the other two: the
      // host registers a subscription at a path that holds nothing.
      final DuetWatch<double> watch = await c().maybeEditor.zoom.watch((_) {});
      expect(watch.current, isA<DuetAbsent<double>>());
      await watch.close();
    });

    test('a child watcher sees absent, then the value, then absent again',
        () async {
      final List<DuetReading<double>> seen = <DuetReading<double>>[];
      final DuetWatch<double> watch =
          await c().maybeEditor.zoom.watch(seen.add);
      expect(watch.current, isA<DuetAbsent<double>>());

      // Some other guest gives the option a value: an ancestor write.
      await client.set('maybe_editor', schema.at('maybe_editor').accept);
      await router.settled();
      expect(watch.current, const DuetPresent<double>(3.25));

      // ...then a write to the leaf itself.
      await client.set('maybe_editor.zoom', const DuetFloat(9.5));
      await router.settled();
      expect(watch.current, const DuetPresent<double>(9.5));

      // ...then back to None, which makes the child absent again.
      await client.set('maybe_editor', const DuetNull());
      await router.settled();
      expect(watch.current, isA<DuetAbsent<double>>());

      expect(
        seen,
        <DuetReading<double>>[
          const DuetPresent<double>(3.25),
          const DuetPresent<double>(9.5),
          const DuetAbsent<double>(),
        ],
      );
      await watch.close();
    });
  }, skip: skip);
}

/// Subscriptions against the real host, with the pushed value pinned exactly.
void _subscriptions(CorpusSchema schema, {required String? skip}) {
  group('a subscription against the real host', () {
    late StdioHost host;
    late DuetClient client;
    late DuetRouter router;
    app.AppClient c() => app.AppClient(router);

    setUpAll(() async {
      host = await StdioHost.start('app');
      client = DuetClient(host);
      router = DuetRouter(client)..attach();
    });

    tearDownAll(() async {
      await host.close();
    });

    setUp(() async {
      await client.set('', schema.seed);
    });

    test('receives a push carrying the exact value the host wrote', () async {
      final List<DuetReading<double>> seen = <DuetReading<double>>[];
      final DuetWatch<double> watch = await c().editor.zoom.watch(seen.add);
      expect(watch.current, const DuetPresent<double>(0));

      await client.set('editor.zoom', const DuetFloat(9.5));
      await router.settled();

      expect(seen, <DuetReading<double>>[const DuetPresent<double>(9.5)]);
      expect(watch.current, const DuetPresent<double>(9.5));
      await watch.close();
    });

    test('sees an ancestor write through the struct it watches', () async {
      // The host sends a patch at the *written* path, never at the watcher's
      // own, so this exercises `duetMergeMirror`'s descend case against real
      // host output rather than against a fake's idea of it.
      final List<DuetReading<app.Editor>> seen = <DuetReading<app.Editor>>[];
      final DuetWatch<app.Editor> watch = await c().editor.self.watch(seen.add);

      await client.set('editor.theme', const DuetStr('nord'));
      await router.settled();

      expect(
        seen,
        <DuetReading<app.Editor>>[
          const DuetPresent<app.Editor>(
            app.Editor(zoom: 0, theme: 'nord'),
          ),
        ],
      );
      await watch.close();
    });

    test('reports a foreign type pushed by another guest as a mismatch',
        () async {
      // A push has no call stack to throw into, so a mismatch has to arrive as
      // data. Against the real host, over a real pipe.
      final List<DuetReading<double>> seen = <DuetReading<double>>[];
      final DuetWatch<double> watch = await c().editor.zoom.watch(seen.add);

      await client.set('editor.zoom', const DuetStr('huge'));
      await router.settled();

      // **Two** readings, not one, and both are the same mismatch. A reading
      // that does not decode makes the router refetch the path — a value this
      // codec refuses may be exactly what the host holds — and the refetch
      // reports what it found, which is the same mismatch again.
      //
      // `generated_client_test.dart` sees only the first because it asserts
      // before the refetch lands; this waits for it with `settled()`, so the
      // count is part of what is pinned rather than a race.
      expect(seen, hasLength(2));
      for (final DuetReading<double> reading in seen) {
        expect(reading, isA<DuetMismatch<double>>());
        expect((reading as DuetMismatch<double>).found, const DuetStr('huge'));
        expect(reading.reason, contains('expected double'));
      }
      await watch.close();
    });

    test('stops delivering once it is closed', () async {
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch = await c().counter.watch(seen.add);
      await watch.close();

      await client.set('counter', const DuetInt(5));
      await router.settled();
      expect(seen, isEmpty);
      expect(host.unmatched, isEmpty);
    });
  }, skip: skip);
}

/// Writes whatever ancestors `path` needs before it can be written to.
///
/// A path below an `Option<Struct>` that is `None` addresses nothing, and
/// `Value::set` never creates intermediate nodes — so writing at it fails until
/// the option holds a struct. Driven entirely from the corpus's own `seed`
/// field, so it materializes exactly the paths the corpus says are absent and
/// no others.
Future<void> _materialize(
  DuetClient client,
  CorpusSchema schema,
  String path,
) async {
  if (schema.at(path).seed != null) return;
  final int dot = path.lastIndexOf('.');
  final String parent = dot < 0 ? '' : path.substring(0, dot);
  await _materialize(client, schema, parent);
  await client.set(parent, schema.at(parent).accept);
}
