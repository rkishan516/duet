/**
 * The generated codecs, checked against `corpus/schema-corpus.json`.
 *
 * Mirrors `packages/duet/test/schema_corpus_test.dart`.
 *
 * # What this catches that a golden test cannot
 *
 * `crates/duet-codegen` compares its output byte-for-byte against
 * `test/generated/`. If the very first run had bound `counter` to
 * `duetFloatCodec`, the golden would have recorded it and every run since would
 * have agreed. A byte comparison cannot notice that a codec is bound to the
 * wrong type.
 *
 * So the input comes from a **different producer**: Rust walks the schema and
 * states, per struct field, one value the type admits and several it must
 * refuse. This file feeds those to the committed codecs.
 *
 * # And what it still cannot reach
 *
 * Everything that is a property of `duet-core`'s **write rules** rather than of
 * a value: whether a path resolves on a real store, whether a `set` at it is
 * accepted, whether a subscription pushes, and what happens below an
 * `Option<Struct>` that is `None`. Those need a host process, and they are
 * `live-host.test.ts`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { duetMap, encodeValueText, parseDuetPath, type DuetValue } from '../src/index.ts';
import { duetValueAt, type DuetCodec } from '../src/typed/index.ts';

import {
  appCodec,
  editorCodec as appEditorCodec,
  unluckyCodec,
} from './generated/app.duet.ts';
import {
  editorCodec as wideEditorCodec,
  outerCodec,
  wideCodec,
} from './generated/wide.duet.ts';
import {
  CORPUS_GENERATOR,
  SCHEMA_CORPUS_VERSION,
  loadSchemaCorpus,
  type CorpusField,
} from './support/schema-corpus.ts';

/**
 * Any generated struct codec, whatever it decodes to.
 *
 * `NonNullable<unknown>` is `DuetCodec`'s own type parameter constraint, so
 * every generated codec is assignable to this without a cast.
 */
type AnyCodec = DuetCodec<NonNullable<unknown>>;

/**
 * Every generated codec, by schema fixture and then by schema type.
 *
 * Hand-written on purpose: this is the one place the corpus's type names meet
 * the generated declarations. The assertion that it covers each schema exactly
 * is what keeps it honest — a schema type added and not listed here fails
 * rather than going unchecked.
 */
const CODECS: Record<string, Record<string, AnyCodec>> = {
  app: { App: appCodec, Editor: appEditorCodec, Unlucky: unluckyCodec },
  wide: { Editor: wideEditorCodec, Outer: outerCodec, Wide: wideCodec },
};

/**
 * The case counts the corpus must contain.
 *
 * Pinned as literals for the same reason `wire-corpus.test.ts` pins its own: a
 * corpus test that consumes whatever it is given proves nothing, and a file
 * truncated to one entry would pass every assertion below.
 */
const SCHEMA_COUNT = 2;
const TYPE_COUNT = 6;
const PATH_COUNT = 31;

const corpus = loadSchemaCorpus();

/** A map holding every field's admitted value, optionally with one replaced. */
function filled(fields: CorpusField[], replace?: string, withValue?: DuetValue): DuetValue {
  return duetMap(
    new Map<string, DuetValue>(
      fields.map((field) => [
        field.key,
        field.key === replace && withValue !== undefined ? withValue : field.accept,
      ]),
    ),
  );
}

describe('the corpus itself', () => {
  test('is the schema this file reads', () => {
    assert.equal(corpus.version, SCHEMA_CORPUS_VERSION);
    assert.equal(
      corpus.generator,
      CORPUS_GENERATOR,
      'the command that regenerates the file must stay accurate',
    );
  });

  test('holds every schema, type and path this file expects', () => {
    assert.equal(corpus.schemas.size, SCHEMA_COUNT);
    let types = 0;
    let paths = 0;
    for (const schema of corpus.schemas.values()) {
      types += schema.types.size;
      paths += schema.paths.length;
    }
    assert.equal(types, TYPE_COUNT);
    assert.equal(paths, PATH_COUNT);
  });

  test('names exactly the types this package generated code for', () => {
    // Both directions. A type in the corpus with no codec here is one nothing
    // checks; a codec here the corpus does not know is one generated from a
    // schema that no longer exists.
    const stated: Record<string, string[]> = {};
    for (const schema of corpus.schemas.values()) {
      stated[schema.name] = [...schema.types.keys()].sort();
    }
    const known: Record<string, string[]> = {};
    for (const [name, codecs] of Object.entries(CODECS)) {
      known[name] = Object.keys(codecs).sort();
    }
    assert.deepStrictEqual(stated, known);
  });
});

for (const schema of corpus.schemas.values()) {
  const codecs = CODECS[schema.name];
  assert.ok(codecs !== undefined, `no codecs for schema ${schema.name}`);
  const root = codecs[schema.root];
  assert.ok(root !== undefined, `no root codec for ${schema.root}`);

  describe(`${schema.name}: the generated codecs`, () => {
    test('decode the seed the host starts from', () => {
      // The value every live-host assertion about an unwritten path is made
      // against. A root codec that could not read it would mean the very first
      // read of a fresh store reported a mismatch.
      assert.notEqual(
        root.decode(schema.seed),
        null,
        `${root.name} cannot decode its own schema's seed`,
      );
    });

    test('re-encode the seed to the exact bytes the corpus states', () => {
      // Decode then encode, compared against text Rust produced. A
      // self-inverting round trip cannot see an encoder and a decoder that are
      // wrong in the same direction; this can.
      const decoded = root.decode(schema.seed);
      assert.notEqual(decoded, null);
      assert.equal(encodeValueText(root.encode(decoded!)), encodeValueText(schema.seed));
    });

    for (const [name, fields] of schema.types) {
      const codec = codecs[name];
      assert.ok(codec !== undefined, `no codec for ${name}`);

      test(`${name} accepts every field value its schema admits`, () => {
        assert.notEqual(
          codec.decode(filled(fields)),
          null,
          `the ${name} codec refused a value its own schema admits; a field ` +
            'bound to the wrong codec fails exactly here',
        );
      });

      test(`${name} occupies exactly the wire keys the schema declares`, () => {
        // The camel-casing check, made against the *encoder*: two guests that
        // disagree about a wire key silently stop seeing each other's writes,
        // and nothing else in this package would notice.
        const decoded = codec.decode(filled(fields));
        assert.notEqual(decoded, null);
        const encoded = codec.encode(decoded!);
        assert.equal(encoded.kind, 'map');
        if (encoded.kind !== 'map') return;
        assert.deepStrictEqual(
          [...encoded.entries.keys()].sort(),
          fields.map((f) => f.key).sort(),
        );
      });

      for (const field of fields) {
        test(`${name}.${field.key} refuses a value of another type`, () => {
          // One field at a time, every other field left admissible, so a
          // refusal can only be about this one.
          for (const reject of field.rejects) {
            assert.equal(
              codec.decode(filled(fields, field.key, reject)),
              null,
              `${field.key} is a ${field.ty}, and the ${name} codec accepted ` +
                encodeValueText(reject),
            );
          }
          // `dynamic` is the one type that refuses nothing, and stating that
          // here stops an empty `rejects` list anywhere else passing as
          // "checked".
          assert.equal(
            field.rejects.length === 0,
            field.ty === 'dynamic',
            'only a dynamic field may have no rejects',
          );
        });
      }
    }
  });

  describe(`${schema.name}: the seed, walked by path`, () => {
    for (const path of schema.paths) {
      test(`"${path.path}" holds what the corpus says it holds`, () => {
        // This package's own path parser and value walker against Rust's, for
        // every path the schema mints. `null` and a `DuetNull` are kept apart
        // deliberately: absent and `None` are different states, and the whole
        // `Option` story rests on that.
        const held = duetValueAt(schema.seed, parseDuetPath(path.path));
        if (path.seed === null) {
          assert.equal(held, null, `"${path.path}" should address nothing`);
        } else {
          assert.notEqual(held, null, `"${path.path}" should hold a value`);
          assert.equal(
            encodeValueText(held!),
            encodeValueText(path.seed),
            `"${path.path}" is a ${path.ty}`,
          );
        }
      });
    }
  });
}
