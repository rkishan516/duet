/**
 * `duetDecodeOutcome` and its three arms.
 *
 * Mirrors `packages/duet/test/typed/duet_outcome_test.dart` case for case. The
 * algorithm behind every generated command method lives here, so this is where
 * the branching is tested — and the arm worth the most attention is the third:
 * a host that answered something the schema's codec cannot read must produce a
 * value the caller can see and log, not a thrown error and not a silent null.
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { duetInt, duetMap, duetNull, duetStr, type DuetValue } from '../../src/index.ts';
import type { DuetInvocation } from '../../src/index.ts';
import {
  duetDecodeOutcome,
  duetDynamicCodec,
  type DuetCodec,
  type DuetOutcome,
} from '../../src/typed/index.ts';

/** A codec that decodes exactly one value and refuses everything else. */
const onlySeven: DuetCodec<bigint> = {
  name: 'OnlySeven',
  encode: (value) => duetInt(value),
  decode: (value) => (value.kind === 'int' && value.value === 7n ? 7n : null),
};

/** A codec for the error side, so the two are never confusable. */
const onlyBoom: DuetCodec<string> = {
  name: 'OnlyBoom',
  encode: (value) => duetStr(value),
  decode: (value) => (value.kind === 'str' && value.value === 'boom' ? 'boom' : null),
};

const returned = (value: DuetValue): DuetInvocation => ({ kind: 'returned', value });
const raised = (error: DuetValue): DuetInvocation => ({ kind: 'raised', error });

describe('duetDecodeOutcome', () => {
  test('a returned value the codec reads becomes ok', () => {
    assert.deepEqual(duetDecodeOutcome(returned(duetInt(7n)), onlySeven, onlyBoom), {
      kind: 'ok',
      value: 7n,
    });
  });

  test('a raised value the codec reads becomes err', () => {
    assert.deepEqual(duetDecodeOutcome(raised(duetStr('boom')), onlySeven, onlyBoom), {
      kind: 'err',
      error: 'boom',
    });
  });

  test('the return codec is never applied to a raised error', () => {
    // The mistake that would pass every happy-path test: a decoder that ran one
    // codec over both arms. `duetInt(7n)` is exactly what the *return* codec
    // accepts, so if it were reached here the answer would be `ok`.
    assert.deepEqual(duetDecodeOutcome(raised(duetInt(7n)), onlySeven, onlyBoom), {
      kind: 'undecodable',
      value: duetInt(7n),
      raised: true,
    });
  });

  test('the raise codec is never applied to a returned value', () => {
    assert.deepEqual(duetDecodeOutcome(returned(duetStr('boom')), onlySeven, onlyBoom), {
      kind: 'undecodable',
      value: duetStr('boom'),
      raised: false,
    });
  });

  test('an undecodable answer says which reply it was', () => {
    // The flag is the whole reason this arm carries more than a value: a
    // `returned` that did not decode may still have succeeded, and a `raised`
    // that did not decode certainly did not.
    const fromReturn = duetDecodeOutcome(returned(duetNull()), onlySeven, onlyBoom);
    const fromRaise = duetDecodeOutcome(raised(duetNull()), onlySeven, onlyBoom);
    assert.equal(fromReturn.kind, 'undecodable');
    assert.equal(fromRaise.kind, 'undecodable');
    assert.notDeepEqual(fromReturn, fromRaise, 'the two must not be one value');
  });

  test('the dynamic codec makes the undecodable arm unreachable', () => {
    // What a command with no declared type is generated with. `dynamic` is the
    // identity, so every answer decodes — which is why a `session.ping` that
    // answers null is an `ok` and not an undecodable.
    for (const answered of [
      duetNull(),
      duetInt(1n),
      duetStr('anything'),
      duetMap(new Map<string, DuetValue>()),
    ]) {
      const outcome: DuetOutcome<DuetValue, DuetValue> = duetDecodeOutcome(
        returned(answered),
        duetDynamicCodec,
        duetDynamicCodec,
      );
      assert.deepEqual(outcome, { kind: 'ok', value: answered });
    }
  });

  test('an ok and an err with equal payloads are still different outcomes', () => {
    // A union whose arms carried only their payload would let a success and a
    // domain failure compare equal, which is the one comparison that must never
    // hold. The `kind` tag is what prevents it.
    const ok = duetDecodeOutcome(returned(duetInt(7n)), onlySeven, duetDynamicCodec);
    const err = duetDecodeOutcome(raised(duetInt(7n)), duetDynamicCodec, duetDynamicCodec);
    assert.notDeepEqual(ok, err);
    assert.equal(ok.kind, 'ok');
    assert.equal(err.kind, 'err');
  });
});
