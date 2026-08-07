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
  DuetFailure,
  duetInt,
  duetStr,
  encodeValueText,
  type DuetInvocation,
  type DuetValue,
} from '../src/index.ts';
import {
  DuetRouter,
  duetAbsent,
  duetNone,
  duetPresent,
  type DuetOutcome,
  type DuetReading,
  type DuetWatch,
} from '../src/typed/index.ts';

import {
  AppClient,
  AppCommands,
  type App,
  type Editor as AppEditor,
  type Unlucky,
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

/**
 * Commands against the real host: the `#[command]` RPC proof.
 *
 * `crates/duet-host-stdio/src/commands.rs` registers three of them, and each
 * exists to be measured from here:
 *
 * - `subtract(a, b)` — arguments arrive under the right names. Subtraction is
 *   not commutative, so a client that swapped them answers `-7n` rather than
 *   `7n`; an `add` would have agreed with a completely broken encoder.
 * - `raise` — a command that ran and returned `Err` reaches this guest as a
 *   `raised` carrying a structured value, not as prose.
 * - `bump(path, by)` — a command body reads and writes the **same store** this
 *   guest reads through its generated accessors.
 *
 * Every assertion names an exact value. A conformance run that only checked "no
 * error was thrown" would pass against a client that sent no arguments at all.
 */
describe('commands, against the real host', { skip }, () => {
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
    assert.deepStrictEqual(host.unmatched, [], 'the host sent lines answering no request');
    await host.close();
  });

  beforeEach(async () => {
    await client.set('', schema.seed);
  });

  test(
    'a command with arguments returns the exact value',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // Arguments deliberately built out of canonical order, so what reaches the
      // host is the encoder's sort rather than this literal's order.
      const outcome = await client.invoke(
        'subtract',
        new Map<string, DuetValue>([
          ['b', duetInt(3n)],
          ['a', duetInt(10n)],
        ]),
      );
      assert.deepStrictEqual(
        outcome,
        { kind: 'returned', value: duetInt(7n) },
        '10 - 3; a client that swapped the argument names answers -7',
      );
    },
  );

  test(
    'a command that returns Err arrives as a typed raise',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The distinction `raised` exists for, end to end over a real pipe: the
      // error is a structured value this guest can match on, and a `failed`
      // would have flattened it into a sentence on the way out.
      const outcome = await client.invoke('raise');
      assert.equal(outcome.kind, 'raised');
      if (outcome.kind !== 'raised') return;
      assert.equal(
        encodeValueText(outcome.error),
        '{"t":"m","v":{"code":{"t":"s","v":"unlucky"},"short_by":{"t":"i","v":"42"}}}',
      );
      assert.equal(outcome.error.kind, 'map');
      if (outcome.error.kind !== 'map') return;
      assert.deepStrictEqual(
        outcome.error.entries.get('short_by'),
        duetInt(42n),
        'the integer field must survive as a bigint',
      );
    },
  );

  test('an unknown command is refused, never raised', { timeout: CASE_TIMEOUT_MS }, async () => {
    // A near-miss of a registered name, so the answer cannot come from the
    // request being malformed. It must arrive as a `DuetFailure` — the host
    // would not run it — and never as a `raised`, which would say something ran
    // and failed.
    await assert.rejects(
      () => client.invoke('subtrac'),
      (error: Error) =>
        error instanceof DuetFailure &&
        error.message === 'no command named "subtrac" is registered for this surface',
    );
  });

  test(
    'malformed arguments are refused with a bounded message',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The right values under the wrong names: exactly what a client whose
      // argument encoder had been renamed would send.
      await assert.rejects(
        () =>
          client.invoke(
            'subtract',
            new Map<string, DuetValue>([
              ['x', duetInt(10n)],
              ['y', duetInt(3n)],
            ]),
          ),
        (error: Error) =>
          error instanceof DuetFailure && error.message === 'argument "a" is missing',
      );

      // ...and a one-megabyte argument must not become a one-megabyte reply. The
      // refusal names the argument and the KIND that arrived, never the value.
      await assert.rejects(
        () =>
          client.invoke(
            'subtract',
            new Map<string, DuetValue>([
              ['a', duetStr('z'.repeat(1_000_000))],
              ['b', duetInt(1n)],
            ]),
          ),
        (error: Error) =>
          error instanceof DuetFailure &&
          error.message === 'argument "a" must be an integer, got a string' &&
          error.message.length < 300,
      );
    },
  );

  test(
    'a command writes the store, and the typed path reads it back',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // THE claim of the whole increment: commands and shared state are one
      // world. The read is through the **generated accessor**, not through the
      // raw client, so a command whose write landed at some other path — or
      // under a camel-cased key — reads nothing here.
      assert.deepStrictEqual(
        await c().counter.get(),
        duetPresent(0n),
        'the premise: the seed leaves counter at 0',
      );

      assert.deepStrictEqual(await bump(client, 'counter', 5n), {
        kind: 'returned',
        value: duetInt(5n),
      });
      assert.deepStrictEqual(await c().counter.get(), duetPresent(5n));

      // Twice, because a body that read a stale copy of the store would still
      // answer 5 the first time.
      assert.deepStrictEqual(await bump(client, 'counter', 5n), {
        kind: 'returned',
        value: duetInt(10n),
      });
      assert.deepStrictEqual(await c().counter.get(), duetPresent(10n));
    },
  );

  test("a command's write reaches a typed watcher", { timeout: CASE_TIMEOUT_MS }, async () => {
    // The same claim through the push path. A host that served commands against
    // some second store would satisfy the read-back test above by accident of
    // ordering and deliver nothing here.
    const seen: DuetReading<bigint>[] = [];
    const watch = await c().counter.watch((r) => seen.push(r));
    assert.deepStrictEqual(watch.current, duetPresent(0n));

    assert.deepStrictEqual(await bump(client, 'counter', 2n), {
      kind: 'returned',
      value: duetInt(2n),
    });
    await router.settled();

    assert.deepStrictEqual(seen, [duetPresent(2n)]);
    assert.deepStrictEqual(watch.current, duetPresent(2n));
    await watch.close();
  });

  test(
    'a command that cannot do its job raises rather than refusing',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The other side of the refused/raised line, decided by the host: the call
      // was well-formed and `title` simply is not an integer. A guest must see
      // that as a command that RAN and failed.
      const outcome = await bump(client, 'title', 1n);
      assert.equal(outcome.kind, 'raised');
      if (outcome.kind !== 'raised') return;
      assert.equal(
        encodeValueText(outcome.error),
        '{"t":"m","v":{"code":{"t":"s","v":"not_an_integer"},"found":{"t":"s","v":"string"}}}',
      );
      // ...and the store is untouched, so the refusal was not cosmetic.
      const held = await client.get('title');
      assert.equal(encodeValueText(held as DuetValue), '{"t":"s","v":""}');
    },
  );
});

/** Invokes the host's `bump` command, which is called from four cases above. */
function bump(client: DuetClient, path: string, by: bigint): Promise<DuetInvocation> {
  return client.invoke(
    'bump',
    new Map<string, DuetValue>([
      ['path', duetStr(path)],
      ['by', duetInt(by)],
    ]),
  );
}

/**
 * The **generated** command client, driven against the real host.
 *
 * The suite above drives `DuetClient.invoke` by hand: a string name and a map of
 * tagged values, written out at each call site. This drives `AppCommands` — the
 * class `duet-codegen` emits from `schema/app.json` — over the same pipe, and it
 * is the only thing in this package that proves the generated names, argument
 * keys and codecs are the ones a live host answers.
 *
 * A golden test cannot make this check. `client.invoke('sessionPing')` is not a
 * syntax error, not a type error and not a decode error; it is a refusal at run
 * time, and a byte comparison would have recorded the camel-cased spelling as
 * the truth forever.
 */
describe('the generated commands class, against the real host', { skip }, () => {
  const schema = schemaNamed(corpus, 'app');
  let host: StdioHost;
  let client: DuetClient;
  let router: DuetRouter;
  const commands = (): AppCommands => new AppCommands(client);

  before(() => {
    host = StdioHost.start('app');
    client = new DuetClient(host);
    router = new DuetRouter(client);
    router.attach();
  });

  after(async () => {
    assert.deepStrictEqual(host.unmatched, [], 'the host sent lines answering no request');
    await host.close();
  });

  beforeEach(async () => {
    await client.set('', schema.seed);
  });

  test(
    'a generated method binds its arguments by key and not by position',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // Subtraction is not commutative, so a generated method that encoded `a`
      // under `b`'s key answers -7 rather than 7. An `add` would have agreed
      // with a completely broken binding.
      assert.deepStrictEqual(await commands().subtract({ a: 10n, b: 3n }), {
        kind: 'ok',
        value: 7n,
      });
      assert.deepStrictEqual(await commands().subtract({ a: 3n, b: 10n }), {
        kind: 'ok',
        value: -7n,
      });
    },
  );

  test(
    'a generated method decodes a raised error into its schema type',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The `raises` type in `schema/app.json` is `Unlucky`, and the generated
      // method binds `unluckyCodec` to it. What arrives is that struct, not a
      // `DuetValue` the caller has to take apart.
      const outcome: DuetOutcome<DuetValue, Unlucky> = await commands().raise();
      assert.equal(outcome.kind, 'err');
      if (outcome.kind !== 'err') return;
      assert.equal(outcome.error.code, 'unlucky');
      assert.equal(
        outcome.error.shortBy,
        42n,
        'the member is camel-cased and the wire key is not',
      );
    },
  );

  test(
    'a dotted command name reaches the host uncamel-cased',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // `session.ping` is the one command whose method name differs from its
      // wire name by more than a case change. The host registers
      // `session.ping`; a client that sent `sessionPing` would be refused, and
      // nothing before this point could have noticed.
      const outcome = await commands().sessionPing();
      assert.equal(outcome.kind, 'ok');
      if (outcome.kind !== 'ok') return;
      assert.equal(
        encodeValueText(outcome.value),
        '{"t":"n"}',
        'a command with no declared result answers null',
      );
    },
  );

  test(
    'a generated command writes the store the generated accessors read',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The whole "commands and state are one world" claim, with both halves
      // generated from one schema.
      const state = new AppClient(router);
      assert.deepStrictEqual(await state.counter.get(), duetPresent(0n));
      assert.deepStrictEqual(await commands().bump({ path: 'counter', by: 5n }), {
        kind: 'ok',
        value: 5n,
      });
      assert.deepStrictEqual(await state.counter.get(), duetPresent(5n));
    },
  );

  test(
    'an unknown command still throws, through the generated class too',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // The refused/ran line, from the generated side. A name the host does not
      // register rejects with `DuetFailure`; it never becomes an `err`, which
      // would say something ran and failed.
      await assert.rejects(
        () => client.invoke('sessionPing'),
        DuetFailure,
        'the camel-cased spelling must not resolve on the host',
      );
    },
  );

  test(
    'a command that ran and failed is an outcome, not a rejection',
    { timeout: CASE_TIMEOUT_MS },
    async () => {
      // `bump`'s declared `raises` is `dynamic`, which is the truth about this
      // host: it raises three differently shaped maps under one name. So the
      // generated method decodes it through `duetDynamicCodec` and the caller
      // gets the raw value — typed as `DuetValue`, which is what the schema
      // says.
      const outcome = await commands().bump({ path: 'title', by: 1n });
      assert.equal(outcome.kind, 'err');
      if (outcome.kind !== 'err') return;
      assert.equal(
        encodeValueText(outcome.error),
        '{"t":"m","v":{"code":{"t":"s","v":"not_an_integer"},"found":{"t":"s","v":"string"}}}',
      );
    },
  );
});
