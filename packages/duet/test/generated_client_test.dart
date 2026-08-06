/// The generated client, driven against a host.
///
/// # Why this exists next to a golden test
///
/// `crates/duet-codegen` compares its output byte-for-byte against the files in
/// `test/generated/`. That proves the emitter still emits what it emitted
/// before; it proves nothing about whether the output is *right*. A generated
/// file that never compiled, or that compiled and reported a mismatch on every
/// read, would pass its golden test forever.
///
/// So the generated code is compiled here — `dart analyze` covers this
/// directory — and then run: every accessor is read and written through a fake
/// host transcribed from `crates/duet-core/src/value.rs`, including its exact
/// refusal messages. A wrong codec, a wrong path or a decoder that cannot read
/// what its own encoder produced fails here rather than in a byte comparison.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

import 'generated/app.duet.dart';
import 'typed/fake_host.dart';

/// The store an `App` occupies, written through the generated codec so the
/// seed and the client cannot disagree about the shape.
DuetValue _seed() => const AppCodec().encode(
      const App(
        counter: 1,
        editor: Editor(zoom: 1.5, theme: 'dark'),
        title: 'untitled',
      ),
    );

/// A client attached to `host`.
Future<AppClient> _client(FakeHost host) async {
  final DuetRouter router = DuetRouter(DuetClient(host))..attach();
  return AppClient(router);
}

void main() {
  group('the generated codec is its own inverse', () {
    test('a round trip through encode and decode is the identity', () {
      const App app = App(
        counter: 42,
        editor: Editor(zoom: 2.5, theme: 'light'),
        title: 'draft',
      );
      final DuetValue encoded = const AppCodec().encode(app);
      expect(const AppCodec().decode(encoded), app);
    });

    test('the encoded map uses the schema keys, not the accessor names', () {
      // The check that would fail if the emitter camel-cased a wire key: the
      // Rust host owns `counter`, and a map keyed `Counter` would be a struct
      // no other guest could read a field out of.
      final DuetValue encoded = const AppCodec().encode(
        const App(
          counter: 1,
          editor: Editor(zoom: 1, theme: 'dark'),
          title: 't',
        ),
      );
      expect(encoded, isA<DuetMap>());
      expect(
        (encoded as DuetMap).entries.keys.toSet(),
        <String>{'counter', 'editor', 'title'},
      );
    });

    test('a decoder refuses a map missing a field rather than inventing one',
        () {
      expect(
        const AppCodec().decode(DuetMap(<String, DuetValue>{
          'counter': const DuetInt(1),
          'title': const DuetStr('t'),
        })),
        isNull,
      );
    });

    test('a decoder refuses a field of the wrong type', () {
      // `Value::Int` and `Value::Float` are distinct on the host. A zoom that
      // arrived as an integer is another guest disagreeing about the schema,
      // and the decode must say so rather than widening it.
      expect(
        const EditorCodec().decode(DuetMap(<String, DuetValue>{
          'zoom': const DuetInt(1),
          'theme': const DuetStr('dark'),
        })),
        isNull,
      );
    });

    test('a decoder refuses a value that is not a map at all', () {
      expect(const AppCodec().decode(const DuetInt(1)), isNull);
    });
  });

  group('every generated accessor reads what the host holds', () {
    test('a scalar at the root', () async {
      final AppClient app = await _client(FakeHost(_seed()));
      expect(await app.counter.get(), const DuetPresent<int>(1));
      expect(await app.title.get(), const DuetPresent<String>('untitled'));
    });

    test('a nested scalar, through the nested accessor class', () async {
      // `app.editor.zoom` addresses `editor.zoom`. The path is a literal in the
      // generated file; this is what proves the literal is the right one.
      final AppClient app = await _client(FakeHost(_seed()));
      expect(await app.editor.zoom.get(), const DuetPresent<double>(1.5));
      expect(await app.editor.theme.get(), const DuetPresent<String>('dark'));
    });

    test('a whole struct, through the nested class\'s own handle', () async {
      final AppClient app = await _client(FakeHost(_seed()));
      expect(
        await app.editor.self.get(),
        const DuetPresent<Editor>(Editor(zoom: 1.5, theme: 'dark')),
      );
    });

    test('the root, through the root client\'s own handle', () async {
      final AppClient app = await _client(FakeHost(_seed()));
      expect(
        await app.self.get(),
        const DuetPresent<App>(
          App(
            counter: 1,
            editor: Editor(zoom: 1.5, theme: 'dark'),
            title: 'untitled',
          ),
        ),
      );
    });
  });

  group('every generated accessor writes where the host expects', () {
    test('a write through a nested accessor lands at the nested path',
        () async {
      // The assertion is against the host's own tree at the *wire* path, not
      // against a read back through the same accessor — which would agree with
      // itself whatever path it used.
      final FakeHost host = FakeHost(_seed());
      final AppClient app = await _client(host);

      await app.editor.zoom.set(3.25);
      expect(host.valueAt('editor.zoom'), const DuetFloat(3.25));
    });

    test('a write of a whole struct replaces every field', () async {
      final FakeHost host = FakeHost(_seed());
      final AppClient app = await _client(host);

      await app.editor.self.set(const Editor(zoom: 4, theme: 'solarized'));
      expect(host.valueAt('editor.zoom'), const DuetFloat(4));
      expect(host.valueAt('editor.theme'), const DuetStr('solarized'));
    });

    test('what another guest wrote at the wire path is what the accessor reads',
        () async {
      // The other direction, and the one a same-accessor round trip cannot
      // reach: a second guest writes using the *schema's* key, and the typed
      // accessor must see it. A camel-cased path would read nothing here.
      final FakeHost host = FakeHost(_seed());
      final AppClient app = await _client(host);

      expect(host.write('editor.theme', const DuetStr('nord')), isNull);
      expect(await app.editor.theme.get(), const DuetPresent<String>('nord'));
    });
  });

  group('a typed watcher stays in step with the host', () {
    test('a watch sees a change another guest made below it', () async {
      final FakeHost host = FakeHost(_seed());
      final AppClient app = await _client(host);

      final List<DuetReading<Editor>> seen = <DuetReading<Editor>>[];
      final DuetWatch<Editor> watch = await app.editor.self.watch(seen.add);
      expect(watch.current, isA<DuetPresent<Editor>>());

      expect(host.write('editor.zoom', const DuetFloat(9)), isNull);
      expect(
        seen,
        <DuetReading<Editor>>[
          const DuetPresent<Editor>(Editor(zoom: 9, theme: 'dark')),
        ],
      );
      await watch.close();
    });

    test('a value another guest wrote of the wrong type is a mismatch, not a '
        'throw', () async {
      // Two guests share one store, so a typed watcher will meet a value its
      // codec refuses. It has to be reported, not raised: a push has no call
      // stack to throw into.
      final FakeHost host = FakeHost(_seed());
      final AppClient app = await _client(host);

      final List<DuetReading<double>> seen = <DuetReading<double>>[];
      final DuetWatch<double> watch = await app.editor.zoom.watch(seen.add);

      expect(host.write('editor.zoom', const DuetStr('huge')), isNull);
      expect(seen.single, isA<DuetMismatch<double>>());
      expect(
        (seen.single as DuetMismatch<double>).reason,
        contains('expected double'),
      );
      await watch.close();
    });
  });
}
