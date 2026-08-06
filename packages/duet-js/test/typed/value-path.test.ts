/**
 * `duetValueAt` and `duetValueWith`: the read and the functional update.
 *
 * Mirrors `packages/duet/test/typed/duet_value_path_test.dart`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  decodeValueText,
  duetBool,
  duetFloat,
  duetInt,
  duetList,
  duetMap,
  duetStr,
  DuetCodecError,
  encodeValueText,
  parseDuetPath,
  DUET_ROOT_PATH,
  type DuetPath,
  type DuetValue,
} from '../../src/index.ts';
import { duetValueAt, duetValueWith } from '../../src/typed/index.ts';

/**
 * A map of maps `depth` levels deep with `leaf` at the bottom, built from the
 * inside out so that constructing the fixture cannot itself overflow.
 */
function nest(depth: number, leaf: DuetValue): DuetValue {
  let current = leaf;
  for (let i = 0; i < depth; i++) {
    current = duetMap(new Map<string, DuetValue>([['k', current]]));
  }
  return current;
}

/** `k.k.k...`, `depth` segments long. */
function nestPath(depth: number): DuetPath {
  return parseDuetPath(new Array<string>(depth).fill('k').join('.'));
}

function tree(): DuetValue {
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
      [
        'documents',
        duetList([
          duetMap(new Map<string, DuetValue>([['title', duetStr('one')]])),
          duetMap(new Map<string, DuetValue>([['title', duetStr('two')]])),
        ]),
      ],
      ['flag', duetBool(true)],
    ]),
  );
}

describe('duetValueAt', () => {
  test('the root path is the whole tree', () => {
    assert.deepStrictEqual(duetValueAt(tree(), DUET_ROOT_PATH), tree());
  });

  test('reads through maps and lists', () => {
    assert.deepStrictEqual(duetValueAt(tree(), parseDuetPath('editor.zoom')), duetFloat(1));
    assert.deepStrictEqual(
      duetValueAt(tree(), parseDuetPath('documents[1].title')),
      duetStr('two'),
    );
  });

  test('a missing key, an out-of-range index and a wrong kind all answer null', () => {
    assert.equal(duetValueAt(tree(), parseDuetPath('editor.nope')), null);
    assert.equal(duetValueAt(tree(), parseDuetPath('documents[2]')), null);
    // A key against a list, an index against a map, and anything at all against
    // a scalar.
    assert.equal(duetValueAt(tree(), parseDuetPath('documents.title')), null);
    assert.equal(duetValueAt(tree(), parseDuetPath('editor[0]')), null);
    assert.equal(duetValueAt(tree(), parseDuetPath('flag.anything')), null);
  });

  test('a hand-built negative index is refused, not thrown at', () => {
    // `parseDuetPath` cannot produce this, but a `DuetPath` is a plain object
    // literal any caller can build, and a bare `items[index]` would hand back
    // `undefined` and be treated as a value.
    const path: DuetPath = {
      segments: [
        { kind: 'key', key: 'documents' },
        { kind: 'index', index: -1 },
      ],
    };
    assert.equal(duetValueAt(tree(), path), null);
  });
});

describe('duetValueWith', () => {
  test('the root path replaces the whole tree and cannot fail', () => {
    assert.deepStrictEqual(duetValueWith(tree(), DUET_ROOT_PATH, duetInt(1n)), duetInt(1n));
  });

  test('overwrites a leaf without touching its siblings', () => {
    const updated = duetValueWith(tree(), parseDuetPath('editor.zoom'), duetFloat(4));
    assert.notEqual(updated, null);
    assert.deepStrictEqual(
      duetValueAt(updated as DuetValue, parseDuetPath('editor.zoom')),
      duetFloat(4),
    );
    assert.deepStrictEqual(
      duetValueAt(updated as DuetValue, parseDuetPath('editor.mode')),
      duetStr('select'),
    );
    assert.deepStrictEqual(
      duetValueAt(updated as DuetValue, parseDuetPath('flag')),
      duetBool(true),
    );
  });

  test('does not mutate the tree it was given', () => {
    const original = tree();
    duetValueWith(original, parseDuetPath('editor.zoom'), duetFloat(9));
    assert.deepStrictEqual(original, tree());
  });

  test('inserts a new key at the final segment of a map path', () => {
    const updated = duetValueWith(tree(), parseDuetPath('editor.grid'), duetBool(false));
    assert.deepStrictEqual(
      duetValueAt(updated as DuetValue, parseDuetPath('editor.grid')),
      duetBool(false),
    );
  });

  test('never creates an intermediate node', () => {
    // `a.b` when `a` is missing is a refusal on the host, not an implicit
    // insert, and a mirror that invented the `a` would drift from it.
    assert.equal(duetValueWith(tree(), parseDuetPath('nope.deeper'), duetInt(1n)), null);
  });

  test('writes into a list in range and refuses one out of range', () => {
    const updated = duetValueWith(
      tree(),
      parseDuetPath('documents[0].title'),
      duetStr('renamed'),
    );
    assert.deepStrictEqual(
      duetValueAt(updated as DuetValue, parseDuetPath('documents[0].title')),
      duetStr('renamed'),
    );
    assert.deepStrictEqual(
      duetValueAt(updated as DuetValue, parseDuetPath('documents[1].title')),
      duetStr('two'),
    );

    // Exactly at the length: a refusal, never an append.
    assert.equal(duetValueWith(tree(), parseDuetPath('documents[2]'), duetInt(1n)), null);
  });

  test('refuses a segment against the wrong kind of node', () => {
    assert.equal(duetValueWith(tree(), parseDuetPath('documents.title'), duetInt(1n)), null);
    assert.equal(duetValueWith(tree(), parseDuetPath('editor[0]'), duetInt(1n)), null);
    assert.equal(duetValueWith(tree(), parseDuetPath('flag.deeper'), duetInt(1n)), null);
  });

  test('a hand-built negative index is refused, not thrown at', () => {
    const path: DuetPath = {
      segments: [
        { kind: 'key', key: 'documents' },
        { kind: 'index', index: -1 },
      ],
    };
    assert.equal(duetValueWith(tree(), path, duetInt(1n)), null);
  });
});

describe('depth', () => {
  test('the fixture really does sit on the wire limit', () => {
    // 63 nested maps encode to 2 containers each, plus 1 for the leaf: 127,
    // exactly MAX_JSON_DEPTH. One more level is 129 and must be refused.
    // Without this assertion the "at the limit" test below would be a test at
    // whatever depth the arithmetic happened to produce.
    const atLimit = encodeValueText(nest(63, duetStr('leaf')));
    assert.equal(decodeValueText(atLimit).kind, 'map');

    // The over-limit text is built by hand rather than encoded, because this
    // package's *encoder* refuses an over-deep document too — a deliberate
    // divergence from the Dart port, which checks depth only on decode (see
    // `writeJson` here and `_requireWireSafe`'s `checkDepth` there). Going
    // through `encodeValueText` would therefore assert the encoder's bound and
    // never reach the decoder's.
    const overLimit = `${'{"t":"m","v":{"k":'.repeat(64)}{"t":"s","v":"leaf"}${'}}'.repeat(64)}`;
    assert.throws(
      () => decodeValueText(overLimit),
      (error: unknown) => error instanceof DuetCodecError && error.reason === 'bad_json',
    );
    // ...and one level shallower, the same hand-built shape is accepted, so the
    // rejection above is the limit talking and not a typo in the text.
    const atLimitText = `${'{"t":"m","v":{"k":'.repeat(63)}{"t":"s","v":"leaf"}${'}}'.repeat(63)}`;
    assert.equal(decodeValueText(atLimitText).kind, 'map');
  });

  test('duetValueWith survives a value at the wire depth limit', () => {
    const deep = nest(63, duetStr('leaf'));
    const path = nestPath(63);

    const updated = duetValueWith(deep, path, duetStr('new'));
    assert.deepStrictEqual(duetValueAt(updated as DuetValue, path), duetStr('new'));
    // And the result is still a legal wire value, so the rebuild did not quietly
    // add a level.
    assert.equal(decodeValueText(encodeValueText(updated as DuetValue)).kind, 'map');
  });

  test('duetValueWith survives a tree far deeper than the wire allows', () => {
    // The wire limit is *not* what makes this test meaningful, and saying so is
    // the point: 63 nested frames would not trouble any stack, so the "at the
    // limit" test above cannot tell a recursive rebuild from an iterative one.
    // This one can. `duetValueWith` is exported, so a caller may hand it a tree
    // built locally that never passed the wire's depth check.
    //
    // Measured on Node v24's default stack: a recursive rebuild of this shape
    // succeeds at 5 000 and throws `RangeError: Maximum call stack size
    // exceeded` at 10 000 and above; the iterative one succeeds at every depth
    // up to 100 000.
    //
    // Nothing here encodes or deep-compares the whole value: `encodeValue` and
    // `duetValueEquals` are both recursive, so asserting on the tree as a whole
    // would overflow in the *test* rather than the code.
    const depth = 100000;
    const deep = nest(depth, duetStr('leaf'));
    const path = nestPath(depth);

    assert.deepStrictEqual(duetValueAt(deep, path), duetStr('leaf'));

    const updated = duetValueWith(deep, path, duetStr('new'));
    assert.notEqual(updated, null);
    assert.deepStrictEqual(duetValueAt(updated as DuetValue, path), duetStr('new'));
  });
});
