/**
 * The generated client, driven against a host.
 *
 * Mirrors `packages/duet/test/generated_client_test.dart`.
 *
 * # Why this exists next to a golden test
 *
 * `crates/duet-codegen` compares its output byte-for-byte against the files in
 * `test/generated/`. That proves the emitter still emits what it emitted before;
 * it proves nothing about whether the output is *right*. A generated file that
 * never compiled, or that compiled and reported a mismatch on every read, would
 * pass its golden test forever.
 *
 * So the generated code is compiled here — `tsc -p tsconfig.test.json` covers
 * this directory — and then run: every accessor is read and written through a
 * fake host transcribed from `crates/duet-core/src/value.rs`, including its
 * exact refusal messages. A wrong codec, a wrong path or a decoder that cannot
 * read what its own encoder produced fails here rather than in a byte
 * comparison.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { DuetClient, duetFloat, duetInt, duetStr, type DuetValue } from '../src/index.ts';
import {
  DuetRouter,
  duetPresent,
  type DuetReading,
  type DuetWatch,
} from '../src/typed/index.ts';

import {
  AppClient,
  appCodec,
  editorCodec,
  type App,
  type Editor,
} from './generated/app.duet.ts';
import { FakeHost } from './typed/fake-host.ts';

/** The `App` every case here starts from. */
const SEED: App = {
  counter: 1n,
  editor: { zoom: 1.5, theme: 'dark' },
  title: 'untitled',
};

/** A store holding {@link SEED}, encoded through the generated codec. */
function seed(): DuetValue {
  return appCodec.encode(SEED);
}

/** A client attached to `host`. */
function client(host: FakeHost): AppClient {
  const router = new DuetRouter(new DuetClient(host));
  router.attach();
  return new AppClient(router);
}

describe('the generated codec is its own inverse', () => {
  test('a round trip through encode and decode is the identity', () => {
    const app: App = {
      counter: 42n,
      editor: { zoom: 2.5, theme: 'light' },
      title: 'draft',
    };
    assert.deepStrictEqual(appCodec.decode(appCodec.encode(app)), app);
  });

  test('the encoded map uses the schema keys, not the accessor names', () => {
    // The check that would fail if the emitter camel-cased a wire key: the Rust
    // host owns `counter`, and a map keyed `Counter` would be a struct no other
    // guest could read a field out of.
    const encoded = appCodec.encode(SEED);
    assert.equal(encoded.kind, 'map');
    if (encoded.kind !== 'map') return;
    assert.deepStrictEqual([...encoded.entries.keys()].sort(), [
      'counter',
      'editor',
      'title',
    ]);
  });

  test('a decoder refuses a map missing a field rather than inventing one', () => {
    const partial = appCodec.encode(SEED);
    assert.equal(partial.kind, 'map');
    if (partial.kind !== 'map') return;
    const missing = new Map(partial.entries);
    missing.delete('editor');
    assert.equal(appCodec.decode({ kind: 'map', entries: missing }), null);
  });

  test('a decoder refuses a field of the wrong type', () => {
    // `Value::Int` and `Value::Float` are distinct on the host. A zoom that
    // arrived as an integer is another guest disagreeing about the schema, and
    // the decode must say so rather than widening it.
    assert.equal(
      editorCodec.decode({
        kind: 'map',
        entries: new Map<string, DuetValue>([
          ['zoom', duetInt(1n)],
          ['theme', duetStr('dark')],
        ]),
      }),
      null,
    );
  });

  test('a decoder refuses a value that is not a map at all', () => {
    assert.equal(appCodec.decode(duetInt(1n)), null);
  });
});

describe('every generated accessor reads what the host holds', () => {
  test('a scalar at the root', async () => {
    const app = client(new FakeHost(seed()));
    assert.deepStrictEqual(await app.counter.get(), duetPresent(1n));
    assert.deepStrictEqual(await app.title.get(), duetPresent('untitled'));
  });

  test('a nested scalar, through the nested accessor class', async () => {
    // `app.editor.zoom` addresses `editor.zoom`. The path is a literal in the
    // generated file; this is what proves the literal is the right one.
    const app = client(new FakeHost(seed()));
    assert.deepStrictEqual(await app.editor.zoom.get(), duetPresent(1.5));
    assert.deepStrictEqual(await app.editor.theme.get(), duetPresent('dark'));
  });

  test("a whole struct, through the nested class's own handle", async () => {
    const app = client(new FakeHost(seed()));
    const expected: Editor = { zoom: 1.5, theme: 'dark' };
    assert.deepStrictEqual(await app.editor.self.get(), duetPresent(expected));
  });

  test("the root, through the root client's own handle", async () => {
    const app = client(new FakeHost(seed()));
    assert.deepStrictEqual(await app.self.get(), duetPresent(SEED));
  });
});

describe('every generated accessor writes where the host expects', () => {
  test('a write through a nested accessor lands at the nested path', async () => {
    // The assertion is against the host's own tree at the *wire* path, not
    // against a read back through the same accessor — which would agree with
    // itself whatever path it used.
    const host = new FakeHost(seed());
    const app = client(host);

    await app.editor.zoom.set(3.25);
    assert.deepStrictEqual(host.valueAt('editor.zoom'), duetFloat(3.25));
  });

  test('a write of a whole struct replaces every field', async () => {
    const host = new FakeHost(seed());
    const app = client(host);

    await app.editor.self.set({ zoom: 4, theme: 'solarized' });
    assert.deepStrictEqual(host.valueAt('editor.zoom'), duetFloat(4));
    assert.deepStrictEqual(host.valueAt('editor.theme'), duetStr('solarized'));
  });

  test('what another guest wrote at the wire path is what the accessor reads', async () => {
    // The other direction, and the one a same-accessor round trip cannot reach:
    // a second guest writes using the *schema's* key, and the typed accessor
    // must see it. A camel-cased path would read nothing here.
    const host = new FakeHost(seed());
    const app = client(host);

    assert.equal(host.write('editor.theme', duetStr('nord')), null);
    assert.deepStrictEqual(await app.editor.theme.get(), duetPresent('nord'));
  });
});

describe('a typed watcher stays in step with the host', () => {
  test('a watch sees a change another guest made below it', async () => {
    const host = new FakeHost(seed());
    const app = client(host);

    const seen: DuetReading<Editor>[] = [];
    const watch: DuetWatch<Editor> = await app.editor.self.watch((reading) =>
      seen.push(reading),
    );
    assert.equal(watch.current.kind, 'present');

    assert.equal(host.write('editor.zoom', duetFloat(9)), null);
    assert.deepStrictEqual(seen, [duetPresent({ zoom: 9, theme: 'dark' })]);
    await watch.close();
  });

  test('a value another guest wrote of the wrong type is a mismatch, not a throw', async () => {
    // Two guests share one store, so a typed watcher will meet a value its
    // codec refuses. It has to be reported, not raised: a push has no call
    // stack to throw into.
    const host = new FakeHost(seed());
    const app = client(host);

    const seen: DuetReading<number>[] = [];
    const watch = await app.editor.zoom.watch((reading) => seen.push(reading));

    assert.equal(host.write('editor.zoom', duetStr('huge')), null);
    assert.equal(seen.length, 1);
    assert.equal(seen[0]?.kind, 'mismatch');
    if (seen[0]?.kind === 'mismatch') {
      assert.ok(seen[0].reason.includes('expected number'));
    }
    await watch.close();
  });
});
