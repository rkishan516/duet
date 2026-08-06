/**
 * `duetMergeMirror`: the three-case merge, tested as a pure function.
 *
 * Mirrors `packages/duet/test/typed/duet_merge_test.dart`.
 *
 * Every test here names the wrong implementation it rules out. That is the
 * discipline this file exists for: a merge test that only ever writes to the
 * watched path itself passes against an implementation that ignores the patch's
 * path entirely, which is the most natural way to get this wrong.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  duetFloat,
  duetInt,
  duetList,
  duetMap,
  duetNull,
  duetStr,
  parseDuetPath,
  DUET_ROOT_PATH,
  type DuetValue,
} from '../../src/index.ts';
import { duetMerged, duetMergeMirror } from '../../src/typed/index.ts';

const p = parseDuetPath;

describe('case 1 — the patch is at the watched path', () => {
  test('the patch is the whole answer', () => {
    assert.deepStrictEqual(
      duetMergeMirror(p('editor.zoom'), duetFloat(1), p('editor.zoom'), duetFloat(2)),
      duetMerged(duetFloat(2)),
    );
  });

  test('it replaces the mirror even when the value did not change', () => {
    // `Store::set` always notifies and never diffs, so a no-op write is a real
    // notification. Nothing here may assume otherwise.
    assert.deepStrictEqual(
      duetMergeMirror(p('a'), duetInt(1n), p('a'), duetInt(1n)),
      duetMerged(duetInt(1n)),
    );
  });

  test('the root watcher of a root write takes the whole tree', () => {
    assert.deepStrictEqual(
      duetMergeMirror(DUET_ROOT_PATH, null, DUET_ROOT_PATH, duetInt(7n)),
      duetMerged(duetInt(7n)),
    );
  });
});

describe('case 2 — the patch is below the watched path', () => {
  test('only the patched leaf changes; the siblings survive', () => {
    // Rules out `mirror = value`, which would answer merged(float 2) — the leaf
    // standing in for the whole struct.
    const merge = duetMergeMirror(
      p('editor'),
      duetMap(
        new Map<string, DuetValue>([
          ['zoom', duetFloat(1)],
          ['mode', duetStr('select')],
        ]),
      ),
      p('editor.zoom'),
      duetFloat(2),
    );
    assert.deepStrictEqual(
      merge,
      duetMerged(
        duetMap(
          new Map<string, DuetValue>([
            ['zoom', duetFloat(2)],
            ['mode', duetStr('select')],
          ]),
        ),
      ),
    );
  });

  test('the relative path is used, not the absolute one', () => {
    // Rules out `duetValueWith(mirror, changed, value)`. That spelling walks
    // `editor` inside the *mirror of* `editor`, finds nothing, and answers
    // resync — so this assertion is the one that separates them.
    const merge = duetMergeMirror(
      p('editor'),
      duetMap(new Map<string, DuetValue>([['zoom', duetFloat(1)]])),
      p('editor.zoom'),
      duetFloat(5),
    );
    assert.deepStrictEqual(
      merge,
      duetMerged(duetMap(new Map<string, DuetValue>([['zoom', duetFloat(5)]]))),
    );
  });

  test('a patch two levels below still lands in the right place', () => {
    const merge = duetMergeMirror(
      p('a'),
      duetMap(
        new Map<string, DuetValue>([
          ['b', duetMap(new Map<string, DuetValue>([['c', duetInt(1n)]]))],
        ]),
      ),
      p('a.b.c'),
      duetInt(2n),
    );
    assert.deepStrictEqual(
      merge,
      duetMerged(
        duetMap(
          new Map<string, DuetValue>([
            ['b', duetMap(new Map<string, DuetValue>([['c', duetInt(2n)]]))],
          ]),
        ),
      ),
    );
  });

  test('an absent mirror resyncs rather than inventing the structure', () => {
    // The null-mirror refetch. Reachable: the host refuses a write below an
    // absent node, so a below-patch arriving against an absent mirror means the
    // mirror is stale, not that the host is wrong.
    const merge = duetMergeMirror(p('editor'), null, p('editor.zoom'), duetFloat(2));
    assert.equal(merge.kind, 'resync');
    assert.match(merge.kind === 'resync' ? merge.reason : '', /no mirror/);
  });

  test('a mirror with no room for the patched path resyncs', () => {
    // The mirror says `editor` is a scalar; the host says it has a `zoom`. One
    // of the two is stale and it is not the host.
    const merge = duetMergeMirror(p('editor'), duetStr('stale'), p('editor.zoom'), duetFloat(2));
    assert.equal(merge.kind, 'resync');
    assert.match(merge.kind === 'resync' ? merge.reason : '', /no room/);
  });
});

describe('case 3 — the patch is above the watched path', () => {
  test('the watched node is read out of the ancestor at the relative path', () => {
    // The single most important assertion in this file, and the reason the
    // ancestor here is `a.b` and not the root.
    //
    // Rules out two wrong implementations at once:
    //   - `mirror = value` answers merged(map{c: 5}) — the ancestor standing in
    //     for the leaf.
    //   - `duetValueAt(value, watched)` — the *absolute* path — walks `a.b.c`
    //     inside a value that is already `a.b`, finds nothing, and answers
    //     merged(null).
    //
    // Both of those agree with the correct answer when the ancestor is the root
    // path, which is why a root-only ancestor test cannot catch either.
    const merge = duetMergeMirror(
      p('a.b.c'),
      duetInt(1n),
      p('a.b'),
      duetMap(new Map<string, DuetValue>([['c', duetInt(5n)]])),
    );
    assert.deepStrictEqual(merge, duetMerged(duetInt(5n)));
  });

  test('a root write is resolved down to the watched leaf', () => {
    const merge = duetMergeMirror(
      p('editor.zoom'),
      duetFloat(1),
      DUET_ROOT_PATH,
      duetMap(
        new Map<string, DuetValue>([
          ['editor', duetMap(new Map<string, DuetValue>([['zoom', duetFloat(3)]]))],
        ]),
      ),
    );
    assert.deepStrictEqual(merge, duetMerged(duetFloat(3)));
  });

  test('an ancestor that no longer contains the path makes it absent, and does not resync', () => {
    // `null` here is a fact, not a gap: the ancestor's value is complete and
    // authoritative for everything under it. Rules out an implementation that
    // treats "not found while descending" as a reason to refetch, which would
    // put a round trip on every `Option` going to `None`.
    assert.deepStrictEqual(
      duetMergeMirror(p('editor.zoom'), duetFloat(1), p('editor'), duetNull()),
      duetMerged(null),
    );
  });

  test('an ancestor replaced by a scalar also makes the path absent', () => {
    assert.deepStrictEqual(
      duetMergeMirror(p('a.b.c'), duetInt(1n), p('a.b'), duetStr('scalar')),
      duetMerged(null),
    );
  });

  test('a list ancestor resolves an indexed watch', () => {
    assert.deepStrictEqual(
      duetMergeMirror(
        p('documents[1].title'),
        duetStr('two'),
        p('documents'),
        duetList([
          duetMap(new Map<string, DuetValue>([['title', duetStr('a')]])),
          duetMap(new Map<string, DuetValue>([['title', duetStr('b')]])),
        ]),
      ),
      duetMerged(duetStr('b')),
    );
  });
});

describe('paths that overlap in neither direction', () => {
  // A conforming host never sends one — `Store::set` only notifies subscriptions
  // whose path overlaps the write. Reaching one means this router and the host
  // disagree about what the subscription watches, and the only answer that
  // converges is to ask.
  test('same length, different path', () => {
    assert.equal(duetMergeMirror(p('a.b'), duetInt(1n), p('a.c'), duetInt(2n)).kind, 'resync');
  });

  test('longer, but not inside the watched subtree', () => {
    assert.equal(duetMergeMirror(p('a.b'), duetInt(1n), p('x.y.z'), duetInt(2n)).kind, 'resync');
  });

  test('shorter, but not an ancestor', () => {
    assert.equal(duetMergeMirror(p('a.b.c'), duetInt(1n), p('x'), duetInt(2n)).kind, 'resync');
  });

  test('a key and an index at the same position do not overlap', () => {
    assert.equal(duetMergeMirror(p('a.b'), duetInt(1n), p('a[0]'), duetInt(2n)).kind, 'resync');
  });
});
