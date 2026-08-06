/**
 * `Option<T>` end to end: the three different things the host does at a child of
 * a `None` struct, and the difference between `None` and "no such path".
 *
 * Mirrors `packages/duet/test/typed/duet_option_test.dart`.
 *
 * Written against behaviour measured on the real host:
 *
 * ```text
 * with editor: Option<Editor> = None
 *   get       at editor.zoom -> null
 *   set       at editor.zoom -> FAILED, "wrong kind of node"
 *   subscribe at editor.zoom -> ok
 * ```
 *
 * The typed layer must not paper over any of the three.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  DuetClient,
  DuetFailure,
  duetFloat,
  duetInt,
  duetMap,
  duetNull,
  duetStr,
  type DuetValue,
} from '../../src/index.ts';
import {
  duetFloatCodec,
  duetIntCodec,
  duetPresent,
  duetStringCodec,
  DuetField,
  DuetOptionalField,
  DuetRouter,
  type DuetReading,
} from '../../src/typed/index.ts';

import { editorCodec, type Editor } from './editor.ts';
import { FakeHost } from './fake-host.ts';

/**
 * The seed: an `Option<Editor>` and an `Option<i64>` both `None`, plus one
 * ordinary field so the tests can tell "absent" from "the store is empty".
 */
function seed(): DuetValue {
  return duetMap(
    new Map<string, DuetValue>([
      ['editor', duetNull()],
      ['count', duetNull()],
      ['title', duetStr('untitled')],
    ]),
  );
}

function attached(host: FakeHost): DuetRouter {
  const router = new DuetRouter(new DuetClient(host));
  router.attach();
  return router;
}

describe('the fake host matches what the real host was measured to do', () => {
  // These three assertions are what make every other test in this file mean
  // something. If the fake ever drifts from the host, this group fails here
  // rather than silently blessing a typed layer built on a wrong assumption.
  test('get at a child of a None struct answers null', async () => {
    const client = new DuetClient(new FakeHost(seed()));
    assert.equal(await client.get('editor.zoom'), null);
  });

  test('set at a child of a None struct is refused as the wrong kind of node', async () => {
    const client = new DuetClient(new FakeHost(seed()));
    await assert.rejects(
      client.set('editor.zoom', duetFloat(2)),
      (error: unknown) =>
        error instanceof DuetFailure &&
        error.message === 'path "editor.zoom" addresses the wrong kind of node',
    );
  });

  test('subscribe at a child of a None struct succeeds', async () => {
    const host = new FakeHost(seed());
    const client = new DuetClient(host);
    const subscription = await client.subscribe('editor.zoom');
    assert.equal(subscription.snapshot, null);
    assert.equal(host.subscriptions.size, 1);
  });
});

describe('DuetOptionalField keeps None and absent apart', () => {
  test('a path holding Value::Null reads as None, not absent', async () => {
    const router = attached(new FakeHost(seed()));
    const editor = new DuetOptionalField(router, 'editor', editorCodec);
    assert.equal((await editor.get()).kind, 'none');
  });

  test('a path that does not exist reads as absent, not None', async () => {
    const router = attached(new FakeHost(seed()));
    const missing = new DuetOptionalField(router, 'nowhere', editorCodec);
    assert.equal((await missing.get()).kind, 'absent');
  });

  test('an Option<i64> distinguishes None from absent as well', async () => {
    const router = attached(new FakeHost(seed()));
    assert.equal((await new DuetOptionalField(router, 'count', duetIntCodec).get()).kind, 'none');
    assert.equal((await new DuetOptionalField(router, 'tally', duetIntCodec).get()).kind, 'absent');
  });

  test('writing null writes Value::Null, which reads back as None', async () => {
    const host = new FakeHost(seed());
    const count = new DuetOptionalField(attached(host), 'count', duetIntCodec);

    await count.set(7n);
    assert.deepStrictEqual(await count.get(), duetPresent(7n));
    assert.deepStrictEqual(host.valueAt('count'), duetInt(7n));

    await count.set(null);
    assert.equal((await count.get()).kind, 'none');
    // Not "the key vanished": the wire has no delete, and `None` is a value.
    assert.deepStrictEqual(host.valueAt('count'), duetNull());
  });

  test('a wrong-typed value is a mismatch, not an exception', async () => {
    const host = new FakeHost(seed());
    const count = new DuetOptionalField(attached(host), 'count', duetIntCodec);

    // Another guest writes a string where the schema says an integer.
    assert.equal(host.write('count', duetStr('lots')), null);

    const reading = await count.get();
    assert.equal(reading.kind, 'mismatch');
    if (reading.kind !== 'mismatch') return;
    assert.deepStrictEqual(reading.found, duetStr('lots'));
    assert.match(reading.reason, /expected bigint/);
  });
});

describe('a required field under a None struct', () => {
  test('reads as absent, and never as None', async () => {
    const router = attached(new FakeHost(seed()));
    const zoom = new DuetField(router, 'editor.zoom', duetFloatCodec);
    assert.equal((await zoom.get()).kind, 'absent');
  });

  test('a write is refused, and says so rather than no-opping', async () => {
    const host = new FakeHost(seed());
    const zoom = new DuetField(attached(host), 'editor.zoom', duetFloatCodec);

    await assert.rejects(
      zoom.set(2),
      (error: unknown) =>
        error instanceof DuetFailure && /wrong kind of node/.test(error.message),
    );
    // And the store really is untouched, so the failure was not cosmetic.
    assert.deepStrictEqual(host.valueAt('editor'), duetNull());
  });

  test('a watch still succeeds, and reports absent', async () => {
    const host = new FakeHost(seed());
    const zoom = new DuetField(attached(host), 'editor.zoom', duetFloatCodec);

    const watch = await zoom.watch(() => {});
    assert.equal(watch.current.kind, 'absent');
    assert.equal(host.subscriptions.size, 1);
  });

  test('a required field holding Value::Null is a mismatch, not None', async () => {
    const host = new FakeHost(seed());
    // `title` exists and is a string; another guest nulls it out.
    assert.equal(host.write('title', duetNull()), null);
    const title = new DuetField(attached(host), 'title', duetStringCodec);

    const reading = await title.get();
    assert.equal(reading.kind, 'mismatch');
    assert.deepStrictEqual(reading.kind === 'mismatch' ? reading.found : null, duetNull());
  });
});

describe('the None/Some transition survives a watch', () => {
  test('a child watcher sees absent, then the value, then absent again', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const zoom = new DuetField(router, 'editor.zoom', duetFloatCodec);

    const seen: DuetReading<number>[] = [];
    const watch = await zoom.watch((reading) => seen.push(reading));
    assert.equal(watch.current.kind, 'absent');

    // Some other guest gives `editor` a value: an ancestor write.
    assert.equal(host.write('editor', editorCodec.encode({ zoom: 2, mode: 'draw' })), null);
    await router.settled();
    // Read into a fresh binding each time: `assert.deepStrictEqual` is a TS
    // assertion signature, so asserting on `watch.current` directly would narrow
    // the getter's type for the rest of the test and make the last check
    // unreachable to the checker.
    const afterAncestor: DuetReading<number> = watch.current;
    assert.deepStrictEqual(afterAncestor, duetPresent(2));

    // ...then a write to the leaf itself.
    assert.equal(host.write('editor.zoom', duetFloat(3)), null);
    await router.settled();
    const afterLeaf: DuetReading<number> = watch.current;
    assert.deepStrictEqual(afterLeaf, duetPresent(3));

    // ...then back to None, which makes the child absent again.
    assert.equal(host.write('editor', duetNull()), null);
    await router.settled();
    const afterNone: DuetReading<number> = watch.current;
    assert.equal(afterNone.kind, 'absent');

    assert.deepStrictEqual(seen.map((r) => r.kind), ['present', 'present', 'absent']);
  });

  test('an optional field watcher reports None and Some separately', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const editor = new DuetOptionalField(router, 'editor', editorCodec);

    const seen: DuetReading<Editor>[] = [];
    const watch = await editor.watch((reading) => seen.push(reading));
    assert.equal(watch.current.kind, 'none');

    const value: Editor = { zoom: 1.5, mode: 'select' };
    assert.equal(host.write('editor', editorCodec.encode(value)), null);
    await router.settled();

    assert.deepStrictEqual(seen, [duetPresent(value)]);
    assert.deepStrictEqual(watch.current, duetPresent(value));
  });
});
