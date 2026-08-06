/**
 * `DuetRouter`: push ownership, id-keyed routing, the early-arrival buffer, and
 * the two ways a watcher recovers from a mirror it cannot trust.
 *
 * Mirrors `packages/duet/test/typed/duet_router_test.dart`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  DuetClient,
  duetFloat,
  duetInt,
  duetMap,
  duetStr,
  parseDuetPath,
  type DuetValue,
} from '../../src/index.ts';
import {
  duetFloatCodec,
  duetIntCodec,
  duetPresent,
  DuetField,
  DuetRouter,
  type DuetReading,
} from '../../src/typed/index.ts';

import { editorCodec, throwingCodec, type Editor } from './editor.ts';
import { FakeHost } from './fake-host.ts';

function seed(): DuetValue {
  return duetMap(
    new Map<string, DuetValue>([
      [
        'editor',
        duetMap(
          new Map<string, DuetValue>([
            ['zoom', duetFloat(1)],
            ['mode', duetStr('select')],
          ]),
        ),
      ],
      ['count', duetInt(1n)],
    ]),
  );
}

const p = parseDuetPath;

function attached(host: FakeHost): DuetRouter {
  const router = new DuetRouter(new DuetClient(host));
  router.attach();
  return router;
}

/** The single subscription id the host has handed out. */
function onlyId(host: FakeHost): bigint {
  return [...host.subscriptions.keys()][0] as bigint;
}

/** One turn of the microtask queue, so an in-flight `get` can be raced. */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe('the push slot has exactly one owner', () => {
  test('attaching over an existing owner is an error, not a silent steal', () => {
    const client = new DuetClient(new FakeHost(seed()));
    // An application that wanted raw pushes for itself.
    client.onPush = () => {};

    assert.throws(() => new DuetRouter(client).attach(), /another owner/);
  });

  test('a second router cannot attach to the same client', () => {
    const client = new DuetClient(new FakeHost(seed()));
    new DuetRouter(client).attach();

    assert.throws(() => new DuetRouter(client).attach(), /another owner/);
  });

  test('attaching the same router twice is an error', () => {
    const router = new DuetRouter(new DuetClient(new FakeHost(seed())));
    router.attach();

    assert.throws(() => router.attach(), /already attached/);
  });

  test('detaching hands the slot back', () => {
    const host = new FakeHost(seed());
    const client = new DuetClient(host);
    const first = new DuetRouter(client);
    first.attach();

    first.detach();
    assert.equal(client.onPush, null);
    assert.equal(host.isListening, false);

    assert.doesNotThrow(() => new DuetRouter(client).attach());
  });

  test('detaching twice is safe', () => {
    const router = new DuetRouter(new DuetClient(new FakeHost(seed())));
    router.attach();
    router.detach();
    assert.doesNotThrow(() => router.detach());
    assert.equal(router.isAttached, false);
  });

  test('watching before attaching is an error, not silence', async () => {
    const router = new DuetRouter(new DuetClient(new FakeHost(seed())));
    const count = new DuetField(router, 'count', duetIntCodec);

    await assert.rejects(count.watch(() => {}), /attach\(\)/);
  });

  test('a detached router stops delivering', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];
    await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));

    router.detach();
    assert.equal(host.write('count', duetInt(9n)), null);
    await router.settled();

    assert.deepStrictEqual(seen, []);
  });
});

describe('routing is keyed by subscription id', () => {
  test('two subscriptions on one path move independently', async () => {
    // The discriminating case for id-keying: both watchers have the *same*
    // path, so an implementation that matched on the path would update both and
    // this test would fail. The host stamps every notification with the
    // subscription it answers; that is the only correct key.
    const host = new FakeHost(seed());
    const router = attached(host);
    const count = new DuetField(router, 'count', duetIntCodec);

    const first = await count.watch(() => {});
    const second = await count.watch(() => {});
    assert.equal(host.subscriptions.size, 2);

    host.pushTo(onlyId(host), p('count'), duetInt(42n));
    await router.settled();

    assert.deepStrictEqual(first.current, duetPresent(42n));
    assert.deepStrictEqual(second.current, duetPresent(1n));
  });

  test('overlapping watchers each merge the same patch their own way', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);

    const struct = await new DuetField(router, 'editor', editorCodec).watch(() => {});
    const leaf = await new DuetField(router, 'editor.zoom', duetFloatCodec).watch(() => {});

    // One write; two notifications; two different merges — below for the
    // struct, at for the leaf.
    assert.equal(host.write('editor.zoom', duetFloat(3)), null);
    await router.settled();

    assert.deepStrictEqual(struct.current, duetPresent<Editor>({ zoom: 3, mode: 'select' }));
    assert.deepStrictEqual(leaf.current, duetPresent(3));
    // Neither needed the host's help.
    assert.equal(host.getCount, 0);
  });

  test('a notification for an unknown id does not disturb a live watcher', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});

    host.pushTo(9999n, p('count'), duetInt(7n));
    await router.settled();

    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });

  test('closing unsubscribes and stops delivery', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];
    const watch = await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));

    await watch.close();
    assert.equal(watch.isClosed, true);
    assert.equal(host.subscriptions.size, 0);

    assert.equal(host.write('count', duetInt(5n)), null);
    await router.settled();
    assert.deepStrictEqual(seen, []);

    // Idempotent.
    await watch.close();
  });
});

describe('a push can arrive before its own subscribed reply', () => {
  test('it is buffered, folded, and not delivered to a handle nobody holds', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];

    // The host registers the subscription and notifies it before the guest has
    // read the reply that names it.
    host.beforeSubscribedReply = (h, id) => {
      h.pushTo(id, p('count'), duetInt(2n));
      h.pushTo(id, p('count'), duetInt(3n));
    };

    const watch = await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));

    // Caught up on return...
    assert.deepStrictEqual(watch.current, duetPresent(3n));
    // ...and the application was not called back for a handle it did not hold
    // yet.
    assert.deepStrictEqual(seen, []);
    assert.equal(host.getCount, 0);
  });

  test('a buffered push is not delivered to a different subscription', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);

    host.beforeSubscribedReply = (h, id) => {
      // A notification for an id that will never register.
      h.pushTo(id + 500n, p('count'), duetInt(99n));
    };
    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});

    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });

  test('the buffer is bounded, and overflow refetches instead of dropping', async () => {
    const host = new FakeHost(seed());
    const router = new DuetRouter(new DuetClient(host), 2);
    router.attach();

    host.beforeSubscribedReply = (h, id) => {
      // Five notifications into a buffer with room for two. The last three
      // cannot be recorded, so folding what *was* recorded would leave a mirror
      // with a hole in it.
      for (let i = 2n; i <= 6n; i++) h.pushTo(id, p('count'), duetInt(i));
      // The store's truth, which none of those pushes carried.
      if (h.root.kind !== 'map') throw new Error('the seed is a map');
      const entries = new Map(h.root.entries);
      entries.set('count', duetInt(77n));
      h.root = duetMap(entries);
    };

    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});
    await router.settled();

    // Not 6 (the last buffered push) and not 3 (the last one that fitted): the
    // host was asked.
    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent(77n));
  });

  test('the id map is bounded too, and every later watcher refetches', async () => {
    const host = new FakeHost(seed());
    const router = new DuetRouter(new DuetClient(host), 1);
    router.attach();

    // Notifications for two ids that will never register: the first fills the id
    // map, the second cannot even be recorded.
    host.pushTo(700n, p('count'), duetInt(2n));
    host.pushTo(800n, p('count'), duetInt(3n));

    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});
    await router.settled();

    // The blunt fallback fired: this watcher had nothing to do with either
    // dropped push, and still refetched rather than risk being wrong.
    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });

  test('a zero-length buffer is legal and simply always refetches', async () => {
    const host = new FakeHost(seed());
    const router = new DuetRouter(new DuetClient(host), 0);
    router.attach();

    host.beforeSubscribedReply = (h, id) => {
      h.pushTo(id, p('count'), duetInt(2n));
    };
    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});
    await router.settled();

    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });

  test('a negative buffer size is refused at construction', () => {
    assert.throws(() => new DuetRouter(new DuetClient(new FakeHost(seed())), -1), RangeError);
  });
});

describe('a mirror that cannot be merged is refetched', () => {
  test('a patch below an absent mirror refetches rather than inventing one', async () => {
    const host = new FakeHost(duetMap(new Map<string, DuetValue>()));
    const router = attached(host);
    const watch = await new DuetField(router, 'editor', editorCodec).watch(() => {});
    assert.equal(watch.current.kind, 'absent');

    // The mirror has gone stale relative to the host: some other guest wrote the
    // whole struct, and this guest is being told only about the leaf. (Injected
    // directly, because a conforming host would have sent the ancestor patch too
    // — the point is that a router which folded this into an absent mirror would
    // fabricate a struct with one field.)
    host.root = duetMap(
      new Map<string, DuetValue>([['editor', editorCodec.encode({ zoom: 4, mode: 'pan' })]]),
    );
    host.pushTo(onlyId(host), p('editor.zoom'), duetFloat(4));
    await router.settled();

    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent<Editor>({ zoom: 4, mode: 'pan' }));
  });

  test('a patch naming a path that does not overlap refetches', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});

    host.pushTo(onlyId(host), p('editor.mode'), duetStr('pan'));
    await router.settled();

    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });

  test('a refetch that fails does not throw into the application', async () => {
    const host = new FakeHost(seed());
    host.refuseGets = true;
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];
    const watch = await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));

    host.pushTo(onlyId(host), p('editor.mode'), duetStr('pan'));
    await router.settled();

    // The last known reading is delivered rather than an exception, and the host
    // is asked once, not once per retry.
    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(seen, [duetPresent(1n)]);
    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });
});

describe('a value the codec refuses is reported and resynced', () => {
  test('a mismatch is delivered, the host is asked once, and the loop stops', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];
    const watch = await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));

    // Another guest writes a string where the schema says an integer.
    assert.equal(host.write('count', duetStr('lots')), null);
    await router.settled();

    // Reported immediately, then confirmed by the refetch...
    assert.deepStrictEqual(seen.map((r) => r.kind), ['mismatch', 'mismatch']);
    assert.equal(watch.current.kind, 'mismatch');
    // ...and asked for exactly once. A resync that itself resynced on a mismatch
    // would spin here forever, one round trip per turn.
    assert.equal(host.getCount, 1);
  });

  test('a mismatch caused by a stale mirror is repaired by the refetch', async () => {
    // The recovery case, not merely the reporting one: the host holds a
    // perfectly good value and this guest's mirror does not.
    const host = new FakeHost(seed());
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];
    const watch = await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));

    host.pushTo(onlyId(host), p('count'), duetStr('garbage'));
    await router.settled();

    assert.equal(seen[0]?.kind, 'mismatch');
    assert.deepStrictEqual(seen[seen.length - 1], duetPresent(1n));
    assert.deepStrictEqual(watch.current, duetPresent(1n));
    assert.equal(host.getCount, 1);
  });

  test('an exception from the application callback cannot cancel the resync', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const watch = await new DuetField(router, 'count', duetIntCodec).watch((reading) => {
      if (reading.kind === 'mismatch') throw new Error('application bug');
    });

    // The push escapes as the application's own exception, which this package
    // deliberately does not swallow...
    assert.throws(
      () => host.pushTo(onlyId(host), p('count'), duetStr('garbage')),
      /application bug/,
    );
    await router.settled();

    // ...and the recovery still happened, because it was scheduled before the
    // callback ran.
    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent(1n));
  });

  test('a codec that throws becomes a mismatch, not an escaping exception', async () => {
    const host = new FakeHost(seed());
    const field = new DuetField(attached(host), 'count', throwingCodec);

    const reading = await field.get();
    assert.equal(reading.kind, 'mismatch');
    assert.match(reading.kind === 'mismatch' ? reading.reason : '', /threw/);
  });
});

describe('a notification that overtakes a refetch wins', () => {
  test('the stale answer is discarded rather than overwriting a newer one', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const watch = await new DuetField(router, 'count', duetIntCodec).watch(() => {});
    const id = onlyId(host);

    // Hold the resync's read open...
    let release = (): void => {};
    host.holdGets = new Promise<void>((resolve) => {
      release = resolve;
    });
    host.pushTo(id, p('editor.mode'), duetStr('pan')); // forces a resync

    // ...deliver a fresher notification inside its round trip...
    await tick();
    host.pushTo(id, p('count'), duetInt(50n));
    assert.deepStrictEqual(watch.current, duetPresent(50n));

    // ...then let the read finish. Its answer is older than the push.
    release();
    await router.settled();

    assert.equal(host.getCount, 1);
    assert.deepStrictEqual(watch.current, duetPresent(50n));
  });

  test('a closed watcher ignores a refetch that was already in flight', async () => {
    const host = new FakeHost(seed());
    const router = attached(host);
    const seen: DuetReading<bigint>[] = [];
    const watch = await new DuetField(router, 'count', duetIntCodec).watch((r) => seen.push(r));
    const id = onlyId(host);

    let release = (): void => {};
    host.holdGets = new Promise<void>((resolve) => {
      release = resolve;
    });
    host.pushTo(id, p('editor.mode'), duetStr('pan'));

    await tick();
    host.holdGets = null;
    await watch.close();
    release();
    await router.settled();

    assert.deepStrictEqual(seen, []);
  });
});
