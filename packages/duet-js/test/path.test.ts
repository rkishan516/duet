/**
 * Paths, mirroring `packages/duet/test/duet_path_test.dart`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  duetPathEquals,
  duetPathIsPrefixOf,
  DuetCodecError,
  DuetReason,
  DUET_ROOT_PATH,
  formatDuetPath,
  MAX_DUET_INDEX,
  parseDuetPath,
} from '../src/index.ts';

test('parsing and formatting are mutually inverse', () => {
  for (const spelling of [
    '',
    'a',
    'a.b',
    'editor.zoom',
    'documents[3]',
    'documents[3].title',
    'a[0][1]',
    '[0]',
    '[0].a',
    'a b',
    ' ',
    '🦀',
    'a.🦀[12].b',
  ]) {
    assert.equal(formatDuetPath(parseDuetPath(spelling)), spelling, spelling);
  }
});

test('the empty path is the root', () => {
  assert.deepStrictEqual(parseDuetPath('').segments, []);
  assert.ok(duetPathEquals(parseDuetPath(''), DUET_ROOT_PATH));
  assert.equal(formatDuetPath(DUET_ROOT_PATH), '');
});

test('segments carry the parsed structure, not the raw text', () => {
  // A client that kept the raw string could "pass" a decode test by echoing its
  // input, and could re-emit any garbage it was handed.
  assert.deepStrictEqual(parseDuetPath('documents[3].title').segments, [
    { kind: 'key', key: 'documents' },
    { kind: 'index', index: 3 },
    { kind: 'key', key: 'title' },
  ]);
});

test('an index may not follow a dot', () => {
  // `a.[0]` is not legal syntax; write `a[0]`. A leading index is fine.
  assert.throws(() => parseDuetPath('a.[0]'), DuetCodecError);
  assert.doesNotThrow(() => parseDuetPath('a[0]'));
  assert.doesNotThrow(() => parseDuetPath('[0]'));
});

test('every malformed path is refused with reason bad_path', () => {
  for (const bad of [
    'a.[0]',
    'a.',
    '.a',
    'a..b',
    'a[',
    'a[]',
    'a[007]',
    'a[+3]',
    'a[-1]',
    'a[1]x',
    'a]',
    'a[1.5]',
  ]) {
    let thrown: unknown;
    try {
      parseDuetPath(bad);
    } catch (error) {
      thrown = error;
    }
    assert.ok(thrown instanceof DuetCodecError, `${bad} must be refused by this package`);
    assert.equal((thrown as DuetCodecError).reason, DuetReason.badPath, bad);
  }
});

test('a key is any run of characters other than . [ ]', () => {
  assert.deepStrictEqual(parseDuetPath('a b').segments, [{ kind: 'key', key: 'a b' }]);
  assert.deepStrictEqual(parseDuetPath(' ').segments, [{ kind: 'key', key: ' ' }]);
  assert.deepStrictEqual(parseDuetPath('🦀').segments, [{ kind: 'key', key: '🦀' }]);
});

test('an index past the exactly-representable range is refused, not rounded', () => {
  // Rust bounds an index by `usize` and Dart by its 64-bit `int`; JavaScript
  // stops at 2^53-1, because past there `Number` no longer holds consecutive
  // integers and the path would render back as a DIFFERENT path.
  assert.equal(MAX_DUET_INDEX, Number.MAX_SAFE_INTEGER);
  assert.equal(Number('9007199254740993'), 9007199254740992, 'the rounding this guards against');
  assert.doesNotThrow(() => parseDuetPath('a[9007199254740991]'));
  assert.throws(() => parseDuetPath('a[9007199254740993]'), DuetCodecError);
  assert.throws(() => parseDuetPath('a[99999999999999999999]'), DuetCodecError);
});

test('prefix matching is the subscription rule', () => {
  const editor = parseDuetPath('editor');
  assert.ok(duetPathIsPrefixOf(editor, parseDuetPath('editor.zoom')));
  assert.ok(duetPathIsPrefixOf(editor, editor));
  assert.ok(duetPathIsPrefixOf(DUET_ROOT_PATH, parseDuetPath('a.b[0]')));
  assert.ok(!duetPathIsPrefixOf(editor, parseDuetPath('editorial')));
  assert.ok(!duetPathIsPrefixOf(parseDuetPath('editor.zoom'), editor));
});

test('equal paths are equal values, whatever built them', () => {
  assert.ok(duetPathEquals(parseDuetPath('a[0]'), { segments: [{ kind: 'key', key: 'a' }, { kind: 'index', index: 0 }] }));
  assert.ok(!duetPathEquals(parseDuetPath('a[0]'), parseDuetPath('a[1]')));
  assert.ok(!duetPathEquals(parseDuetPath('a'), parseDuetPath('[0]')));
});
