/**
 * The value codec, mirroring `packages/duet/test/duet_value_test.dart`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  decodeValue,
  decodeValueText,
  duetBool,
  duetBytes,
  duetFloat,
  duetInt,
  duetList,
  duetMap,
  duetNull,
  duetStr,
  duetValueEquals,
  DuetCodecError,
  DuetReason,
  encodeValueText,
  MAX_JSON_DEPTH,
  type DuetValue,
} from '../src/index.ts';

/** The IEEE-754 bits of `v`, the only comparison that is exact for every double. */
function bits(v: number): string {
  const view = new DataView(new ArrayBuffer(8));
  view.setFloat64(0, v);
  return view.getBigUint64(0).toString(16).padStart(16, '0');
}

/** Asserts that `run` throws this package's error, for `reason`. */
function refuses(run: () => unknown, reason: string, what: string): void {
  let thrown: unknown;
  let threw = false;
  try {
    run();
  } catch (error) {
    thrown = error;
    threw = true;
  }
  assert.ok(threw, `${what} must be refused`);
  assert.ok(thrown instanceof DuetCodecError, `${what} must be refused by this package: ${String(thrown)}`);
  assert.equal((thrown as DuetCodecError).reason, reason, `${what}: wrong reason (${String(thrown)})`);
}

describe('tagged encoding', () => {
  test('matches the duet-codec goldens byte for byte', () => {
    const goldens: readonly (readonly [DuetValue, string])[] = [
      [duetNull(), '{"t":"n"}'],
      [duetBool(true), '{"t":"bool","v":true}'],
      [duetInt(0n), '{"t":"i","v":"0"}'],
      [duetInt(-1n), '{"t":"i","v":"-1"}'],
      [duetStr('hi'), '{"t":"s","v":"hi"}'],
      [duetBytes(new Uint8Array([102, 111, 111])), '{"t":"b","v":"Zm9v"}'],
      [duetList([]), '{"t":"l","v":[]}'],
      [duetMap(new Map()), '{"t":"m","v":{}}'],
    ];
    for (const [value, wire] of goldens) {
      assert.equal(encodeValueText(value), wire);
    }
  });

  test('an Int above 2^53 survives as a decimal string', () => {
    // The reason DuetInt is a bigint. As a `number`, 9007199254740993 rounds to
    // 9007199254740992 the moment it is parsed, and re-encodes wrong with no
    // error anywhere.
    const wire = '{"t":"i","v":"9007199254740993"}';
    const decoded = decodeValueText(wire);
    assert.equal(decoded.kind, 'int');
    assert.equal((decoded as { value: bigint }).value, 9007199254740993n);
    assert.equal(encodeValueText(decoded), wire);
    // And the proof that a `number`-backed client could not have done it.
    assert.equal(Number('9007199254740993'), 9007199254740992);
  });

  test('i64 bounds round-trip, and one past them is refused', () => {
    for (const spelling of ['9223372036854775807', '-9223372036854775808']) {
      const wire = `{"t":"i","v":"${spelling}"}`;
      assert.equal(encodeValueText(decodeValueText(wire)), wire);
    }
    refuses(() => decodeValueText('{"t":"i","v":"9223372036854775808"}'), DuetReason.badInt, 'i64::MAX + 1');
    refuses(() => duetInt(9223372036854775808n), DuetReason.badInt, 'constructing past i64::MAX');
  });
});

describe('rejections', () => {
  test('a JSON-number Int is rejected, matching Rust', () => {
    refuses(() => decodeValueText('{"t":"i","v":42}'), DuetReason.badInt, 'a JSON-number int');
    refuses(() => decodeValueText('{"t":"i","v":"007"}'), DuetReason.badInt, 'a non-canonical int');
    refuses(() => decodeValueText('{"t":"i","v":"+1"}'), DuetReason.badInt, 'a leading-plus int');
  });

  test('every malformed value is refused by this package, never by JSON.parse', () => {
    const cases: readonly (readonly [string, string])[] = [
      ['not json at all', DuetReason.badJson],
      ['{"t":"q","v":1}', DuetReason.unknownTag],
      ['{}', DuetReason.badShape],
      ['{"t":"i"}', DuetReason.badShape],
      ['{"t":1,"v":1}', DuetReason.badShape],
      ['[]', DuetReason.badShape],
      ['null', DuetReason.badShape],
      ['{"t":"bool","v":1}', DuetReason.badShape],
      ['{"t":"s","v":1}', DuetReason.badShape],
      ['{"t":"l","v":{}}', DuetReason.badShape],
      ['{"t":"m","v":[]}', DuetReason.badShape],
      ['{"t":"f","v":"huge"}', DuetReason.badFloat],
      ['{"t":"f","v":null}', DuetReason.badFloat],
      ['{"t":"b","v":"!!!"}', DuetReason.badBase64],
      ['{"t":"b","v":1}', DuetReason.badBase64],
    ];
    for (const [wire, reason] of cases) {
      refuses(() => decodeValueText(wire), reason, wire);
    }
  });

  test('error messages are bounded, whatever the peer sends', () => {
    // A one-megabyte payload must not become a one-megabyte log line on a hot
    // IPC path.
    const huge = 'z'.repeat(100_000);
    let message = '';
    try {
      decodeValue({ t: huge });
    } catch (error) {
      message = (error as Error).message;
    }
    assert.ok(message.length < 200, `error message was ${String(message.length)} characters long`);
  });

  test('an over-long integer payload is refused without a BigInt conversion', () => {
    // Canonical-looking, and far outside the domain. Refused on length, so the
    // decoder never allocates a million-digit BigInt for an untrusted peer.
    const huge = `9${'0'.repeat(1_000_000)}`;
    refuses(() => decodeValue({ t: 'i', v: huge }), DuetReason.badInt, 'a million-digit int');
  });
});

describe('nesting', () => {
  test('decoding refuses nesting past MAX_JSON_DEPTH, as serde_json does', () => {
    const nest = (depth: number): string =>
      '{"t":"l","v":['.repeat(depth) + '{"t":"n"}' + ']}'.repeat(depth);
    // Each level costs two containers (the object and its array), so the
    // shallowest refused nesting is a little over half MAX_JSON_DEPTH levels.
    assert.doesNotThrow(() => decodeValueText(nest(60)));
    refuses(() => decodeValueText(nest(200)), DuetReason.badJson, 'a 200-level value');
  });

  test('the guard survives a tree deep enough to overflow a recursive walk', () => {
    // The whole point of the iterative depth check. `JSON.parse` accepts this
    // happily — V8 has no depth limit at all — so a recursive guard would blow
    // the stack on exactly the input it exists to reject, and a RangeError is
    // not this package's error type.
    const deep = '['.repeat(50_000) + ']'.repeat(50_000);
    assert.doesNotThrow(() => JSON.parse(deep) as unknown, 'JSON.parse has no depth limit');
    refuses(() => decodeValueText(deep), DuetReason.badJson, 'a 50 000-deep document');
  });

  test('MAX_JSON_DEPTH is the serde_json limit, stated once', () => {
    assert.equal(MAX_JSON_DEPTH, 128);
  });
});

describe('floats', () => {
  test('the three JSON-impossible doubles travel as sentinels', () => {
    // JSON.stringify turns all three into `null` without a word, so the
    // sentinels are mandatory, not cosmetic.
    assert.equal(JSON.stringify(NaN), 'null');
    assert.equal(encodeValueText(duetFloat(NaN)), '{"t":"f","v":"NaN"}');
    assert.equal(encodeValueText(duetFloat(Infinity)), '{"t":"f","v":"Infinity"}');
    assert.equal(encodeValueText(duetFloat(-Infinity)), '{"t":"f","v":"-Infinity"}');
  });

  test('negative zero encodes as a string sentinel', () => {
    // The divergence this sentinel exists for: JavaScript cannot write -0 as a
    // JSON number at all.
    assert.equal(JSON.stringify(-0), '0');
    assert.equal(encodeValueText(duetFloat(-0)), '{"t":"f","v":"-0"}');
    // And +0 must NOT be tagged. `n === -0` is true for +0, which is why the
    // encoder uses Object.is.
    assert.equal(encodeValueText(duetFloat(0)), '{"t":"f","v":0}');
  });

  test('negative zero round-trips with its sign', () => {
    for (const wire of ['{"t":"f","v":"-0"}', '{"t":"f","v":-0.0}']) {
      const decoded = decodeValueText(wire);
      assert.equal(bits((decoded as { value: number }).value), '8000000000000000');
      assert.equal(encodeValueText(decoded), '{"t":"f","v":"-0"}');
    }
  });

  test('the float decoder is wider than the encoder', () => {
    // A guest hand-building a value cannot force a decimal point —
    // JSON.stringify(1.0) is "1" — so the decoder accepts any JSON number.
    const decoded = decodeValueText('{"t":"f","v":1}');
    assert.equal(bits((decoded as { value: number }).value), '3ff0000000000000');
    for (const [wire, expected] of [
      ['{"t":"f","v":"NaN"}', '7ff8000000000000'],
      ['{"t":"f","v":"Infinity"}', '7ff0000000000000'],
      ['{"t":"f","v":"-Infinity"}', 'fff0000000000000'],
    ] as const) {
      assert.equal(bits((decodeValueText(wire) as { value: number }).value), expected);
    }
  });
});

describe('equality', () => {
  test('the container variants compare by content, not identity', () => {
    assert.ok(duetValueEquals(duetList([duetInt(1n)]), duetList([duetInt(1n)])));
    assert.ok(!duetValueEquals(duetList([duetInt(1n)]), duetList([duetInt(2n)])));
    assert.ok(
      duetValueEquals(
        duetMap({ a: duetStr('x'), b: duetNull() }),
        duetMap(new Map<string, DuetValue>([['b', duetNull()], ['a', duetStr('x')]])),
      ),
      'map equality must not depend on insertion order',
    );
    assert.ok(
      duetValueEquals(duetBytes(new Uint8Array([1, 2])), duetBytes(new Uint8Array([1, 2]))),
    );
  });

  test('float equality is IEEE-754, which is why the corpus uses bits', () => {
    assert.ok(!duetValueEquals(duetFloat(NaN), duetFloat(NaN)), 'NaN equals nothing');
    assert.ok(duetValueEquals(duetFloat(-0), duetFloat(0)), '-0 equals 0 under ==');
    assert.notEqual(bits(-0), bits(0), 'but their bits differ, which is what the corpus compares');
  });
});

test('an absent value and a null value stay distinguishable', () => {
  // JSON null means "no value at this path"; {"t":"n"} means "it exists and
  // holds null". A client that collapsed the two would lose the distinction the
  // wire draws.
  assert.equal(decodeValueText('{"t":"n"}').kind, 'null');
  refuses(() => decodeValueText('null'), DuetReason.badShape, 'a bare JSON null as a value');
});
