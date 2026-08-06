/**
 * The generated client, driven against the **real Rust host**.
 *
 * Mirrors `packages/duet/test/live_host_test.dart`.
 *
 * # The gap this closes
 *
 * `generated-client.test.ts` drives the same generated code against
 * `test/typed/fake-host.ts` — a TypeScript stand-in transcribed by hand from
 * `crates/duet-core/src/value.rs`, refusal messages included. That covers the
 * codecs and the wire text. It cannot cover the one thing a transcription can
 * get wrong, which is the transcription: a fake that refuses a write the real
 * host accepts, or spells a refusal differently, passes its own tests forever.
 * It did exactly that until this file existed — see the `store rejected the
 * write:` prefix asserted below.
 *
 * So this file spawns `crates/duet-host-stdio`, which wraps
 * `duet_protocol::handle_text` in a process speaking newline-delimited JSON,
 * and drives the **committed goldens** against it over a real pipe.
 *
 * # Every assertion pins an exact value
 *
 * A conformance run that spawned a host and asserted "no error" would pass with
 * a completely broken type mapping. The two directions are pinned against two
 * different producers:
 *
 * - a **typed write** is read back raw at the schema's own wire path and
 *   compared against the wire text Rust put in `corpus/schema-corpus.json`;
 * - a **raw write** of that same Rust-produced text is read back through the
 *   typed accessor and compared against a TypeScript value written by hand here.
 *
 * A wrong path literal fails the first. A wrong codec fails the second. A
 * camel-cased wire key fails both.
 *
 * # Timeouts
 *
 * `node --test` has **no default per-test timeout**, and a previous increment
 * wedged a whole run on that. Every `test()` below carries an explicit one, and
 * `support/live-host.ts` bounds each request independently.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { after, before, beforeEach, describe, test } from 'node:test';

import {
  DuetClient,
  duetStr,
  encodeValueText,
  type DuetValue,
} from '../src/index.ts';
import {
  DuetRouter,
  duetAbsent,
  duetNone,
  duetPresent,
  type DuetReading,
  type DuetWatch,
} from '../src/typed/index.ts';

import {
  AppClient,
  type App,
  type Editor as AppEditor,
} from './generated/app.duet.ts';
import {
  WideClient,
  type Editor as WideEditor,
  type Outer,
  type Wide,
} from './generated/wide.duet.ts';
import { StdioHost, liveHostSkip } from './support/live-host.ts';
import {
  loadSchemaCorpus,
  pathAt,
  schemaNamed,
  type CorpusSchema,
} from './support/schema-corpus.ts';

/** Every test's own ceiling. See this module's header. */
const CASE_TIMEOUT_MS = 30_000;

const skip = liveHostSkip();
const corpus = loadSchemaCorpus();

/**
 * Any reading, whatever it holds.
 *
 * `NonNullable<unknown>` is `DuetReading`'s own type parameter constraint, so
 * every typed reading is assignable to this without a cast.
 */
type AnyReading = DuetReading<NonNullable<unknown>>;

/** One generated accessor, and the exact value it must carry both ways. */
interface Accessor {
  /** The wire path the corpus states for this accessor. */
  readonly path: string;
  /** Writes the value the corpus admits, through the typed accessor. */
  readonly write: () => Promise<void>;
  /** Reads through the typed accessor. */
  readonly read: () => Promise<AnyReading>;
  /** Asserts `read`'s answer is exactly the expected typed value. */
  readonly check: (reading: AnyReading) => void;
}

/** Asserts `reading` is present and holds exactly `value`. */
function present(reading: AnyReading, value: unknown): void {
  assert.equal(reading.kind, 'present', `expected a present value, got ${reading.kind}`);
  if (reading.kind !== 'present') return;
  assert.deepStrictEqual(reading.value, value);
}

const APP_EDITOR: AppEditor = { zoom: 3.25, theme: 'sample' };
const WIDE_EDITOR: WideEditor = { zoom: 3.25, theme: 'sample' };
const OUTER: Outer = { inner: WIDE_EDITOR, depth: 7n };

/** The `App` the corpus admits at the root. */
const APP: App = { counter: 7n, editor: APP_EDITOR, title: 'sample' };

/** The `Wide` the corpus admits at the root. */
const WIDE: Wide = {
  flag: true,
  count: 7n,
  ratio: 3.25,
  label: 'sample',
  blob: Uint8Array.from([1, 2, 3]),
  anything: duetStr('anything'),
  maybeLabel: 'sample',
  maybeRatios: [3.25],
  maybeEditor: WIDE_EDITOR,
  tags: ['sample'],
  matrix: [[7n]],
  lookup: new Map([['k', 7n]]),
  editors: new Map([['k', WIDE_EDITOR]]),
  blobs: [Uint8Array.from([1, 2, 3])],
  loose: new Map<string, DuetValue>([['k', duetStr('anything')]]),
  flags: [true],
  outer: OUTER,
  snakeCaseField: 'sample',
};

/** Every accessor `app.duet.ts` generates. */
function appAccessors(router: () => DuetRouter): Accessor[] {
  const c = (): AppClient => new AppClient(router());
  return [
    {
      path: '',
      write: () => c().self.set(APP),
      read: () => c().self.get(),
      check: (r) => {
        present(r, APP);
      },
    },
    {
      path: 'counter',
      write: () => c().counter.set(7n),
      read: () => c().counter.get(),
      check: (r) => {
        present(r, 7n);
      },
    },
    {
      path: 'editor',
      write: () => c().editor.self.set(APP_EDITOR),
      read: () => c().editor.self.get(),
      check: (r) => {
        present(r, APP_EDITOR);
      },
    },
    {
      path: 'editor.theme',
      write: () => c().editor.theme.set('sample'),
      read: () => c().editor.theme.get(),
      check: (r) => {
        present(r, 'sample');
      },
    },
    {
      path: 'editor.zoom',
      write: () => c().editor.zoom.set(3.25),
      read: () => c().editor.zoom.get(),
      check: (r) => {
        present(r, 3.25);
      },
    },
    {
      path: 'title',
      write: () => c().title.set('sample'),
      read: () => c().title.get(),
      check: (r) => {
        present(r, 'sample');
      },
    },
  ];
}

/** Every accessor `wide.duet.ts` generates. */
function wideAccessors(router: () => DuetRouter): Accessor[] {
  const c = (): WideClient => new WideClient(router());
  const one = (
    path: string,
    write: (client: WideClient) => Promise<void>,
    read: (client: WideClient) => Promise<AnyReading>,
    value: unknown,
  ): Accessor => ({
    path,
    write: () => write(c()),
    read: () => read(c()),
    check: (r) => {
      present(r, value);
    },
  });

  return [
    one('', (x) => x.self.set(WIDE), (x) => x.self.get(), WIDE),
    one(
      'anything',
      (x) => x.anything.set(duetStr('anything')),
      (x) => x.anything.get(),
      duetStr('anything'),
    ),
    one(
      'blob',
      (x) => x.blob.set(Uint8Array.from([1, 2, 3])),
      (x) => x.blob.get(),
      Uint8Array.from([1, 2, 3]),
    ),
    one(
      'blobs',
      (x) => x.blobs.set([Uint8Array.from([1, 2, 3])]),
      (x) => x.blobs.get(),
      [Uint8Array.from([1, 2, 3])],
    ),
    one('count', (x) => x.count.set(7n), (x) => x.count.get(), 7n),
    one(
      'editors',
      (x) => x.editors.set(new Map([['k', WIDE_EDITOR]])),
      (x) => x.editors.get(),
      new Map([['k', WIDE_EDITOR]]),
    ),
    one('flag', (x) => x.flag.set(true), (x) => x.flag.get(), true),
    one('flags', (x) => x.flags.set([true]), (x) => x.flags.get(), [true]),
    one('label', (x) => x.label.set('sample'), (x) => x.label.get(), 'sample'),
    one(
      'lookup',
      (x) => x.lookup.set(new Map([['k', 7n]])),
      (x) => x.lookup.get(),
      new Map([['k', 7n]]),
    ),
    one(
      'loose',
      (x) => x.loose.set(new Map<string, DuetValue>([['k', duetStr('anything')]])),
      (x) => x.loose.get(),
      new Map<string, DuetValue>([['k', duetStr('anything')]]),
    ),
    one('matrix', (x) => x.matrix.set([[7n]]), (x) => x.matrix.get(), [[7n]]),
    one(
      'maybe_editor',
      (x) => x.maybeEditor.self.set(WIDE_EDITOR),
      (x) => x.maybeEditor.self.get(),
      WIDE_EDITOR,
    ),
    one(
      'maybe_editor.theme',
      (x) => x.maybeEditor.theme.set('sample'),
      (x) => x.maybeEditor.theme.get(),
      'sample',
    ),
    one(
      'maybe_editor.zoom',
      (x) => x.maybeEditor.zoom.set(3.25),
      (x) => x.maybeEditor.zoom.get(),
      3.25,
    ),
    one(
      'maybe_label',
      (x) => x.maybeLabel.set('sample'),
      (x) => x.maybeLabel.get(),
      'sample',
    ),
    one(
      'maybe_ratios',
      (x) => x.maybeRatios.set([3.25]),
      (x) => x.maybeRatios.get(),
      [3.25],
    ),
    one('outer', (x) => x.outer.self.set(OUTER), (x) => x.outer.self.get(), OUTER),
    one('outer.depth', (x) => x.outer.depth.set(7n), (x) => x.outer.depth.get(), 7n),
    one(
      'outer.inner',
      (x) => x.outer.inner.self.set(WIDE_EDITOR),
      (x) => x.outer.inner.self.get(),
      WIDE_EDITOR,
    ),
    one(
      'outer.inner.theme',
      (x) => x.outer.inner.theme.set('sample'),
      (x) => x.outer.inner.theme.get(),
      'sample',
    ),
    one(
      'outer.inner.zoom',
      (x) => x.outer.inner.zoom.set(3.25),
      (x) => x.outer.inner.zoom.get(),
      3.25,
    ),
    one('ratio', (x) => x.ratio.set(3.25), (x) => x.ratio.get(), 3.25),
    one(
      'snake_case_field',
      (x) => x.snakeCaseField.set('sample'),
      (x) => x.snakeCaseField.get(),
      'sample',
    ),
    one('tags', (x) => x.tags.set(['sample']), (x) => x.tags.get(), ['sample']),
  ];
}

/**
 * Writes whatever ancestors `path` needs before it can be written to.
 *
 * A path below an `Option<Struct>` that is `None` addresses nothing, and
 * `Value::set` never creates intermediate nodes — so writing at it fails until
 * the option holds a struct. Driven entirely from the corpus's own `seed` field,
 * so it materializes exactly the paths the corpus says are absent and no others.
 */
async function materialize(
  client: DuetClient,
  schema: CorpusSchema,
  path: string,
): Promise<void> {
  if (pathAt(schema, path).seed !== null) return;
  const dot = path.lastIndexOf('.');
  const parent = dot < 0 ? '' : path.slice(0, dot);
  await materialize(client, schema, parent);
  await client.set(parent, pathAt(schema, parent).accept);
}

/** The accessor sweep for one schema. */
function conformance(
  name: string,
  schema: CorpusSchema,
  build: (router: () => DuetRouter) => Accessor[],
): void {
  describe(`${name}, against the real host`, { skip }, () => {
    let host: StdioHost;
    let client: DuetClient;
    let router: DuetRouter;
    // Built now, at declaration time, because the case list is what `test()`
    // calls are generated from. Every closure resolves `router` when it runs.
    const accessors = build(() => router);

    before(() => {
      host = StdioHost.start(name);
      client = new DuetClient(host);
      router = new DuetRouter(client);
      router.attach();
    });

    after(async () => {
      assert.deepStrictEqual(
        host.unmatched,
        [],
        'the host sent lines answering no request; a transport that ' +
          "mis-correlates would hand this client another call's answer",
      );
      await host.close();
    });

    // Every case starts from the seed, so no case can be made to pass by an
    // earlier one's write.
    beforeEach(async () => {
      await client.set('', schema.seed);
    });

    test('every path in the corpus has an accessor, and the reverse', () => {
      // The coverage assertion. Without it the sweep below tests whatever
      // happens to be in the table, and a field added to the schema would be
      // generated, committed and never driven against a host.
      assert.deepStrictEqual(
        accessors.map((a) => a.path).sort(),
        schema.paths.map((p) => p.path).sort(),
      );
    });

    test('the store starts at the seed the corpus states', { timeout: CASE_TIMEOUT_MS }, async () => {
      const fresh = StdioHost.start(name);
      try {
        const reader = new DuetClient(fresh);
        const held = await reader.get('');
        assert.notEqual(held, null);
        assert.equal(encodeValueText(held!), encodeValueText(schema.seed));
      } finally {
        await fresh.close();
      }
    });

    for (const accessor of accessors) {
      const expected = pathAt(schema, accessor.path);

      test(
        `"${accessor.path}" writes where the schema says`,
        { timeout: CASE_TIMEOUT_MS },
        async () => {
          // Typed write, then a **raw** read at the schema's own wire path,
          // compared against the wire text Rust generated. Reading back through
          // the same accessor would agree with itself whatever path it used.
          await materialize(client, schema, accessor.path);
          await accessor.write();
          const held = await client.get(accessor.path);
          assert.notEqual(held, null, `nothing at "${accessor.path}" after the write`);
          assert.equal(
            encodeValueText(held!),
            encodeValueText(expected.accept),
            `the accessor for "${accessor.path}" wrote somewhere else, or ` +
              `encoded a ${expected.ty} differently than the host does`,
          );
        },
      );

      test(
        `"${accessor.path}" reads what another guest wrote`,
        { timeout: CASE_TIMEOUT_MS },
        async () => {
          // The cross-guest direction: a raw write at the wire path, exactly as
          // a second guest sharing this store would make it, read back through
          // the typed accessor. A camel-cased path literal reads nothing here.
          await materialize(client, schema, accessor.path);
          await client.set(accessor.path, expected.accept);
          accessor.check(await accessor.read());
        },
      );

      if (expected.rejects.length > 0) {
        test(
          `"${accessor.path}" reports a foreign type as a mismatch`,
          { timeout: CASE_TIMEOUT_MS },
          async () => {
            // Two guests share one store, so a typed accessor will meet a value
            // its codec refuses. That must be reported, not thrown, and not
            // silently widened into a wrong value.
            for (const reject of expected.rejects) {
              await client.set('', schema.seed);
              await materialize(client, schema, accessor.path);
              await client.set(accessor.path, reject);

              const reading = await accessor.read();
              assert.equal(
                reading.kind,
                'mismatch',
                `"${accessor.path}" is a ${expected.ty} and read ` +
                  `${encodeValueText(reject)} as ${reading.kind}`,
              );
              if (reading.kind !== 'mismatch') return;
              assert.equal(encodeValueText(reading.found), encodeValueText(reject));
            }
          },
        );
      }
    }
  });
}

conformance('app', schemaNamed(corpus, 'app'), appAccessors);
conformance('wide', schemaNamed(corpus, 'wide'), wideAccessors);

describe('an Option<Struct> that is None, against the real host', { skip }, () => {
  const schema = schemaNamed(corpus, 'wide');
  let host: StdioHost;
  let client: DuetClient;
  let router: DuetRouter;
  const c = (): WideClient => new WideClient(router);

  before(() => {
    host = StdioHost.start('wide');
    client = new DuetClient(host);
    router = new DuetRouter(client);
    router.attach();
  });

  after(async () => {
    await host.close();
  });

  beforeEach(async () => {
    await client.set('', schema.seed);
  });

  test('the seed really does leave it None, and its children absent', () => {
    // The premise. Without it every assertion below would be about a state the
    // host was never in.
    assert.equal(pathAt(schema, 'maybe_editor').seed?.kind, 'null');
    assert.equal(pathAt(schema, 'maybe_editor.zoom').seed, null);
  });

  test('the option itself reads None, and never absent', { timeout: CASE_TIMEOUT_MS }, async () => {
    assert.deepStrictEqual(await c().maybeEditor.self.get(), duetNone());
  });

  test('a child get answers absent, and never None', { timeout: CASE_TIMEOUT_MS }, async () => {
    // Measured on the real host: `get` at a child of a `None` struct answers
    // null, which this layer reports as absent. `None` would be a lie — the
    // path holds nothing at all.
    assert.deepStrictEqual(await c().maybeEditor.zoom.get(), duetAbsent());
  });

  test(
    "a child set fails, carrying the host's own refusal",
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // THE assertion this whole increment exists for. `fake-host.ts`
      // transcribes this message; here it comes from the host itself, so a
      // transcription that had drifted fails. It had: the `store rejected the
      // write:` prefix is `duet_runtime::RuntimeError::Store`'s, and the fake
      // omitted it until this file existed.
      await assert.rejects(
        () => c().maybeEditor.zoom.set(2),
        (error: Error) =>
          error.message ===
          'store rejected the write: path "maybe_editor.zoom" addresses the wrong kind of node',
      );
      // ...and the store really is untouched, so the refusal was not cosmetic.
      const held = await client.get('maybe_editor');
      assert.equal(held?.kind, 'null');
    },
  );

  test('a child subscribe succeeds, and reports absent', { timeout: CASE_TIMEOUT_MS }, async () => {
    // The third behaviour, and the one that differs from the other two: the
    // host registers a subscription at a path that holds nothing.
    const watch = await c().maybeEditor.zoom.watch(() => {});
    assert.equal(watch.current.kind, 'absent');
    await watch.close();
  });

  test(
    'a child watcher sees absent, then the value, then absent again',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      const seen: DuetReading<number>[] = [];
      const watch: DuetWatch<number> = await c().maybeEditor.zoom.watch((r) => seen.push(r));
      assert.equal(watch.current.kind, 'absent');

      // Some other guest gives the option a value: an ancestor write.
      await client.set('maybe_editor', pathAt(schema, 'maybe_editor').accept);
      await router.settled();
      assert.deepStrictEqual(watch.current, duetPresent(3.25));

      // ...then a write to the leaf itself.
      await client.set('maybe_editor.zoom', { kind: 'float', value: 9.5 });
      await router.settled();
      assert.deepStrictEqual(watch.current, duetPresent(9.5));

      // ...then back to None, which makes the child absent again.
      await client.set('maybe_editor', { kind: 'null' });
      await router.settled();
      assert.deepStrictEqual(watch.current, duetAbsent());

      assert.deepStrictEqual(seen, [duetPresent(3.25), duetPresent(9.5), duetAbsent()]);
      await watch.close();
    },
  );
});

describe('a subscription against the real host', { skip }, () => {
  const schema = schemaNamed(corpus, 'app');
  let host: StdioHost;
  let client: DuetClient;
  let router: DuetRouter;
  const c = (): AppClient => new AppClient(router);

  before(() => {
    host = StdioHost.start('app');
    client = new DuetClient(host);
    router = new DuetRouter(client);
    router.attach();
  });

  after(async () => {
    await host.close();
  });

  beforeEach(async () => {
    await client.set('', schema.seed);
  });

  test(
    'receives a push carrying the exact value the host wrote',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      const seen: DuetReading<number>[] = [];
      const watch = await c().editor.zoom.watch((r) => seen.push(r));
      assert.deepStrictEqual(watch.current, duetPresent(0));

      await client.set('editor.zoom', { kind: 'float', value: 9.5 });
      await router.settled();

      assert.deepStrictEqual(seen, [duetPresent(9.5)]);
      assert.deepStrictEqual(watch.current, duetPresent(9.5));
      await watch.close();
    },
  );

  test(
    'sees an ancestor write through the struct it watches',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The host sends a patch at the *written* path, never at the watcher's
      // own, so this exercises `duetMergeMirror`'s descend case against real
      // host output rather than against a fake's idea of it.
      const seen: DuetReading<AppEditor>[] = [];
      const watch = await c().editor.self.watch((r) => seen.push(r));

      await client.set('editor.theme', duetStr('nord'));
      await router.settled();

      assert.deepStrictEqual(seen, [duetPresent({ zoom: 0, theme: 'nord' })]);
      await watch.close();
    },
  );

  test(
    'reports a foreign type pushed by another guest as a mismatch',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // A push has no call stack to throw into, so a mismatch has to arrive as
      // data. Against the real host, over a real pipe.
      //
      // **Two** readings, not one, and both are the same mismatch. A reading
      // that does not decode makes the router refetch the path — a value this
      // codec refuses may be exactly what the host holds — and the refetch
      // reports what it found, which is the same mismatch again.
      // `generated-client.test.ts` sees only the first because it asserts
      // before the refetch lands; this waits for it with `settled()`.
      const seen: DuetReading<number>[] = [];
      const watch = await c().editor.zoom.watch((r) => seen.push(r));

      await client.set('editor.zoom', duetStr('huge'));
      await router.settled();

      assert.equal(seen.length, 2);
      for (const reading of seen) {
        assert.equal(reading.kind, 'mismatch');
        if (reading.kind !== 'mismatch') continue;
        assert.deepStrictEqual(reading.found, duetStr('huge'));
        assert.ok(reading.reason.includes('expected number'));
      }
      await watch.close();
    },
  );

  test('stops delivering once it is closed', { timeout: CASE_TIMEOUT_MS }, async () => {
    const seen: DuetReading<bigint>[] = [];
    const watch = await c().counter.watch((r) => seen.push(r));
    await watch.close();

    await client.set('counter', { kind: 'int', value: 5n });
    await router.settled();
    assert.deepStrictEqual(seen, []);
    assert.deepStrictEqual(host.unmatched, []);
  });
});
