/**
 * `DuetCodec`, the primitive codecs, and the four readings they produce.
 *
 * Mirrors `packages/duet/test/typed/duet_codec_test.dart`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  duetBool,
  duetBytes,
  duetFloat,
  duetInt,
  duetList,
  duetMap,
  duetNull,
  duetStr,
  type DuetValue,
} from '../../src/index.ts';
import {
  duetAbsent,
  duetBoolCodec,
  duetBytesCodec,
  duetFloatCodec,
  duetIntCodec,
  duetListCodec,
  duetMapCodec,
  duetNone,
  duetOptionalReading,
  duetPresent,
  duetReadingValue,
  duetRequiredReading,
  duetStringCodec,
  type DuetCodec,
} from '../../src/typed/index.ts';

import { editorCodec, throwingCodec, type Editor } from './editor.ts';

describe('primitive codecs round-trip', () => {
  test('boolean, bigint, number, string and bytes', () => {
    assert.deepStrictEqual(duetBoolCodec.encode(true), duetBool(true));
    assert.equal(duetBoolCodec.decode(duetBool(true)), true);

    assert.deepStrictEqual(duetIntCodec.encode(7n), duetInt(7n));
    assert.equal(duetIntCodec.decode(duetInt(7n)), 7n);

    assert.deepStrictEqual(duetFloatCodec.encode(1.5), duetFloat(1.5));
    assert.equal(duetFloatCodec.decode(duetFloat(1.5)), 1.5);

    assert.deepStrictEqual(duetStringCodec.encode('a'), duetStr('a'));
    assert.equal(duetStringCodec.decode(duetStr('a')), 'a');

    const bytes = new Uint8Array([1, 2]);
    assert.deepStrictEqual(duetBytesCodec.encode(bytes), duetBytes(bytes));
    assert.deepStrictEqual(duetBytesCodec.decode(duetBytes(bytes)), bytes);
  });

  test('every codec refuses a value of another tag', () => {
    assert.equal(duetBoolCodec.decode(duetInt(1n)), null);
    assert.equal(duetStringCodec.decode(duetBytes(new Uint8Array([1]))), null);
    assert.equal(duetBytesCodec.decode(duetStr('a')), null);
    assert.equal(duetIntCodec.decode(duetNull()), null);
  });

  test('an integer is not silently widened to a double, or the reverse', () => {
    // `Value::Int` and `Value::Float` are distinct host types. A codec that
    // accepted one for the other would let a guest write an `i64` where the
    // schema says `f64` and have every other guest agree with it.
    assert.equal(duetFloatCodec.decode(duetInt(1n)), null);
    assert.equal(duetIntCodec.decode(duetFloat(1)), null);
  });

  test('a list is refused whole if any element is', () => {
    const codec = duetListCodec(duetIntCodec);
    assert.equal(codec.name, 'bigint[]');
    assert.deepStrictEqual(codec.decode(duetList([duetInt(1n), duetInt(2n)])), [1n, 2n]);
    // Not `[1n]`: a partial list is a wrong answer that looks like a right one.
    assert.equal(codec.decode(duetList([duetInt(1n), duetStr('x')])), null);
    assert.equal(codec.decode(duetInt(1n)), null);
  });

  test('a map is refused whole if any value is', () => {
    const codec = duetMapCodec(duetIntCodec);
    assert.equal(codec.name, 'Map<string, bigint>');
    assert.deepStrictEqual(
      codec.decode(duetMap(new Map<string, DuetValue>([['a', duetInt(1n)]]))),
      new Map([['a', 1n]]),
    );
    assert.equal(
      codec.decode(
        duetMap(
          new Map<string, DuetValue>([
            ['a', duetInt(1n)],
            ['b', duetStr('x')],
          ]),
        ),
      ),
      null,
    );
    assert.equal(codec.decode(duetInt(1n)), null);
  });

  test('a nested codec round-trips a struct', () => {
    const editor: Editor = { zoom: 2, mode: 'draw' };
    assert.deepStrictEqual(editorCodec.decode(editorCodec.encode(editor)), editor);
    assert.equal(editorCodec.decode(duetInt(1n)), null);
  });
});

describe('readings', () => {
  test('a required field: absent, present, mismatch', () => {
    assert.deepStrictEqual(duetRequiredReading(duetIntCodec, null), duetAbsent());
    assert.deepStrictEqual(duetRequiredReading(duetIntCodec, duetInt(3n)), duetPresent(3n));
    // A required field promised a `T`, and a null is not one.
    assert.equal(duetRequiredReading(duetIntCodec, duetNull()).kind, 'mismatch');
    assert.equal(duetRequiredReading(duetIntCodec, duetStr('x')).kind, 'mismatch');
  });

  test('an optional field: absent, none, present, mismatch', () => {
    assert.deepStrictEqual(duetOptionalReading(duetIntCodec, null), duetAbsent());
    assert.deepStrictEqual(duetOptionalReading(duetIntCodec, duetNull()), duetNone());
    assert.deepStrictEqual(duetOptionalReading(duetIntCodec, duetInt(3n)), duetPresent(3n));
    assert.equal(duetOptionalReading(duetIntCodec, duetStr('x')).kind, 'mismatch');
  });

  test('none and absent are not equal to each other', () => {
    // The whole distinction in one assertion: if these two ever collapse,
    // `Option<T> = None` and "the struct holding it is itself None" become the
    // same answer.
    assert.notDeepStrictEqual(duetNone(), duetAbsent());
  });

  test('a mismatch carries what was actually found', () => {
    const reading = duetRequiredReading(duetIntCodec, duetStr('x'));
    assert.equal(reading.kind, 'mismatch');
    if (reading.kind !== 'mismatch') return;
    assert.deepStrictEqual(reading.found, duetStr('x'));
    assert.match(reading.reason, /expected bigint/);
  });

  test('a throwing codec becomes a mismatch, not an escaping exception', () => {
    const reading = duetRequiredReading(throwingCodec, duetInt(1n));
    assert.equal(reading.kind, 'mismatch');
    assert.match(reading.kind === 'mismatch' ? reading.reason : '', /threw/);
  });

  test('duetReadingValue answers only for a present reading', () => {
    assert.equal(duetReadingValue(duetPresent(3n)), 3n);
    assert.equal(duetReadingValue<bigint>(duetNone()), null);
    assert.equal(duetReadingValue<bigint>(duetAbsent()), null);
  });
});

describe("DuetCodec's non-nullable bound", () => {
  test('a nullable type argument is rejected at the definition site', () => {
    // `@ts-expect-error` is a two-way assertion, and that is what makes this a
    // real pin rather than a comment: `npm test` runs `tsc` over this file
    // first, so the build fails if `DuetCodec<string | null>` compiles *and*
    // fails if this directive is ever left dangling over a line that does not
    // error. Drop `extends {}` from `DuetCodec` and this file stops compiling.
    // @ts-expect-error - `string | null` does not satisfy `T extends {}`
    type Nullable = DuetCodec<string | null>;
    // The alias is referenced so the declaration is not elided before the
    // checker sees it.
    const unused: Nullable | null = null;
    assert.equal(unused, null);
  });

  test('a non-nullable type argument compiles (the control)', () => {
    // Without this, a broken harness that rejected everything would satisfy the
    // test above while proving nothing.
    const good: DuetCodec<string> = duetStringCodec;
    assert.equal(good.name, 'string');
  });
});
