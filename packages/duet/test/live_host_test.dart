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
  _commands(corpus['app'], skip: skip);
  _generatedCommands(corpus['app'], skip: skip);
}

/// Commands against the real host: the `#[command]` RPC proof.
///
/// `crates/duet-host-stdio/src/commands.rs` registers three of them, and each
/// exists to be measured from here:
///
/// - `subtract(a, b)` — arguments arrive under the right names. Subtraction is
///   not commutative, so a client that swapped them answers `-7` rather than
///   `7`; an `add` would have agreed with a completely broken encoder.
/// - `raise` — a command that ran and returned `Err` reaches this guest as a
///   `DuetRaised` carrying a structured value, not as prose.
/// - `bump(path, by)` — a command body reads and writes the **same store** this
///   guest reads through its generated accessors.
///
/// Every assertion names an exact value. A conformance run that only checked
/// "no error was thrown" would pass against a client that sent no arguments at
/// all.
void _commands(CorpusSchema schema, {required String? skip}) {
  group('commands, against the real host', () {
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
      expect(host.unmatched, isEmpty,
          reason: 'the host sent lines answering no request');
      await host.close();
    });

    setUp(() async {
      await client.set('', schema.seed);
    });

    test('a command with arguments returns the exact value', () async {
      // Arguments deliberately built out of canonical order, so what reaches
      // the host is the encoder's sort rather than this literal's order.
      expect(
        await client.invoke('subtract', const <String, DuetValue>{
          'b': DuetInt(3),
          'a': DuetInt(10),
        }),
        const DuetReturned(DuetInt(7)),
        reason: '10 - 3; a client that swapped the argument names answers -7',
      );
    });

    test('a command that returns Err arrives as a typed raise', () async {
      // The distinction `raised` exists for, end to end over a real pipe: the
      // error is a structured value this guest can match on, and a `failed`
      // would have flattened it into a sentence on the way out.
      final DuetInvocation outcome = await client.invoke('raise');
      expect(
        outcome,
        const DuetRaised(
          DuetMap(<String, DuetValue>{
            'code': DuetStr('unlucky'),
            'short_by': DuetInt(42),
          }),
        ),
      );
      final DuetValue error = (outcome as DuetRaised).error;
      expect((error as DuetMap).entries['short_by'], const DuetInt(42),
          reason: 'the integer field must survive as an integer');
    });

    test('an unknown command is refused, never raised', () async {
      // A near-miss of a registered name, so the answer cannot come from the
      // request being malformed. It must arrive as a `DuetFailure` — the host
      // would not run it — and never as a `DuetRaised`, which would say
      // something ran and failed.
      await expectLater(
        client.invoke('subtrac'),
        throwsA(
          isA<DuetFailure>().having(
            (DuetFailure e) => e.message,
            'message',
            'no command named "subtrac" is registered for this surface',
          ),
        ),
      );
    });

    test('malformed arguments are refused with a bounded message', () async {
      // The right values under the wrong names: exactly what a client whose
      // argument encoder had been renamed would send.
      await expectLater(
        client.invoke('subtract', const <String, DuetValue>{
          'x': DuetInt(10),
          'y': DuetInt(3),
        }),
        throwsA(
          isA<DuetFailure>().having(
            (DuetFailure e) => e.message,
            'message',
            'argument "a" is missing',
          ),
        ),
      );

      // ...and a one-megabyte argument must not become a one-megabyte reply.
      // The refusal names the argument and the KIND that arrived, never the
      // value.
      try {
        await client.invoke('subtract', <String, DuetValue>{
          'a': DuetStr('z' * 1000000),
          'b': const DuetInt(1),
        });
        fail('a string where an integer is required must be refused');
      } on DuetFailure catch (e) {
        expect(e.message, 'argument "a" must be an integer, got a string');
        expect(e.message.length, lessThan(300));
      }
    });

    test('a command writes the store, and the typed path reads it back',
        () async {
      // THE claim of the whole increment: commands and shared state are one
      // world. The read is through the **generated accessor**, not through the
      // raw client, so a command whose write landed at some other path — or
      // under a camel-cased key — reads nothing here.
      expect(await c().counter.get(), const DuetPresent<int>(0),
          reason: 'the premise: the seed leaves counter at 0');

      expect(
        await client.invoke('bump', const <String, DuetValue>{
          'path': DuetStr('counter'),
          'by': DuetInt(5),
        }),
        const DuetReturned(DuetInt(5)),
      );
      expect(await c().counter.get(), const DuetPresent<int>(5));

      // Twice, because a body that read a stale copy of the store would still
      // answer 5 the first time.
      expect(
        await client.invoke('bump', const <String, DuetValue>{
          'path': DuetStr('counter'),
          'by': DuetInt(5),
        }),
        const DuetReturned(DuetInt(10)),
      );
      expect(await c().counter.get(), const DuetPresent<int>(10));
    });

    test('a command\'s write reaches a typed watcher', () async {
      // The same claim through the push path. A host that served commands
      // against some second store would satisfy the read-back test above by
      // accident of ordering and deliver nothing here.
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch = await c().counter.watch(seen.add);
      expect(watch.current, const DuetPresent<int>(0));

      expect(
        await client.invoke('bump', const <String, DuetValue>{
          'path': DuetStr('counter'),
          'by': DuetInt(2),
        }),
        const DuetReturned(DuetInt(2)),
      );
      await router.settled();

      expect(seen, <DuetReading<int>>[const DuetPresent<int>(2)]);
      expect(watch.current, const DuetPresent<int>(2));
      await watch.close();
    });

    test('a command that cannot do its job raises rather than refusing',
        () async {
      // The other side of the refused/raised line, decided by the host: the
      // call was well-formed and `title` simply is not an integer. A guest must
      // see that as a command that RAN and failed.
      expect(
        await client.invoke('bump', const <String, DuetValue>{
          'path': DuetStr('title'),
          'by': DuetInt(1),
        }),
        const DuetRaised(
          DuetMap(<String, DuetValue>{
            'code': DuetStr('not_an_integer'),
            'found': DuetStr('string'),
          }),
        ),
      );
      // ...and the store is untouched, so the refusal was not cosmetic.
      expect(await client.get('title'), const DuetStr(''));
    });
  }, skip: skip);
}

/// The **generated** command client, driven against the real host.
///
/// `_commands` above drives `DuetClient.invoke` by hand: a string name and a map
/// of tagged values, written out at each call site. This drives
/// `AppCommands` — the class `duet-codegen` emits from `schema/app.json` — over
/// the same pipe, and it is the only thing in this repository that proves the
/// generated names, argument keys and codecs are the ones a live host answers.
///
/// A golden test cannot make this check. `client.invoke('sessionPing')` is not a
/// syntax error, not a type error and not a decode error; it is a refusal at run
/// time, and a byte comparison would have recorded the camel-cased spelling as
/// the truth forever.
void _generatedCommands(CorpusSchema schema, {required String? skip}) {
  group('the generated commands class, against the real host', () {
    late StdioHost host;
    late DuetClient client;
    late DuetRouter router;
    app.AppCommands commands() => app.AppCommands(client);

    setUpAll(() async {
      host = await StdioHost.start('app');
      client = DuetClient(host);
      router = DuetRouter(client)..attach();
    });

    tearDownAll(() async {
      expect(host.unmatched, isEmpty,
          reason: 'the host sent lines answering no request');
      await host.close();
    });

    setUp(() async {
      await client.set('', schema.seed);
    });

    test('a generated method binds its arguments by key and not by position',
        () async {
      // Subtraction is not commutative, so a generated method that encoded `a`
      // under `b`'s key answers -7 rather than 7. An `add` would have agreed
      // with a completely broken binding.
      expect(await commands().subtract(a: 10, b: 3), const DuetOk<int, DuetValue>(7));
      expect(await commands().subtract(a: 3, b: 10), const DuetOk<int, DuetValue>(-7));
    });

    test('a generated method decodes a raised error into its schema type',
        () async {
      // The `raises` type in `schema/app.json` is `Unlucky`, and the generated
      // method binds `UnluckyCodec` to it. What arrives is that struct, not a
      // `DuetValue` the caller has to take apart.
      final DuetOutcome<DuetValue, app.Unlucky> outcome = await commands().raise();
      expect(outcome, isA<DuetErr<DuetValue, app.Unlucky>>());
      final app.Unlucky error = (outcome as DuetErr<DuetValue, app.Unlucky>).error;
      expect(error.code, 'unlucky');
      expect(error.shortBy, 42,
          reason: 'the accessor is camel-cased and the wire key is not');
    });

    test('a dotted command name reaches the host uncamel-cased', () async {
      // `session.ping` is the one command whose method name differs from its
      // wire name by more than a case change. The host registers
      // `session.ping`; a client that sent `sessionPing` would be refused, and
      // nothing before this point could have noticed.
      expect(
        await commands().sessionPing(),
        const DuetOk<DuetValue, DuetValue>(DuetNull()),
        reason: 'a command with no declared result answers null',
      );
    });

    test('a generated command writes the store the generated accessors read',
        () async {
      // The whole "commands and state are one world" claim, with both halves
      // generated from one schema.
      final app.AppClient state = app.AppClient(router);
      expect(await state.counter.get(), const DuetPresent<int>(0));

      expect(
        await commands().bump(path: 'counter', by: 5),
        const DuetOk<int, DuetValue>(5),
      );
      expect(await state.counter.get(), const DuetPresent<int>(5));
    });

    test('an unknown command still throws, through the generated class too',
        () async {
      // The refused/ran line, from the generated side. A method whose name the
      // host does not register throws `DuetFailure`; it never becomes a
      // `DuetErr`, which would say something ran and failed.
      await expectLater(
        client.invoke('sessionPing'),
        throwsA(isA<DuetFailure>()),
        reason: 'the camel-cased spelling must not resolve on the host',
      );
    });

    test('a command that ran and failed is an outcome, not a thrown failure',
        () async {
      // `bump`'s declared `raises` is `dynamic`, which is the truth about this
      // host: it raises three differently shaped maps under one name. So the
      // generated method decodes it through `duetDynamicCodec` and the caller
      // gets the raw value — typed as `DuetValue`, which is what the schema
      // says.
      final DuetOutcome<int, DuetValue> outcome =
          await commands().bump(path: 'title', by: 1);
      expect(
        outcome,
        const DuetErr<int, DuetValue>(
          DuetMap(<String, DuetValue>{
            'code': DuetStr('not_an_integer'),
            'found': DuetStr('string'),
          }),
        ),
      );
    });
  }, skip: skip);
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
