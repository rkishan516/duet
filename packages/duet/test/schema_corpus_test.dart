/// The generated codecs, checked against `corpus/schema-corpus.json`.
///
/// # What this catches that a golden test cannot
///
/// `crates/duet-codegen` compares its output byte-for-byte against
/// `test/generated/`. If the very first run had bound `counter` to
/// `duetFloatCodec`, the golden would have recorded it and every run since
/// would have agreed. A byte comparison cannot notice that a codec is bound to
/// the wrong type.
///
/// So the input comes from a **different producer**: Rust walks the schema and
/// states, per struct field, one value the type admits and several it must
/// refuse. This file feeds those to the committed codecs. An `int` field
/// reading through a float codec fails on the accept value; a float field
/// reading through an int codec fails on the reject value `{"t":"f","v":1.0}`;
/// a camel-cased wire key fails on the key-set assertion.
///
/// # And what it still cannot reach
///
/// Everything that is a property of `duet-core`'s **write rules** rather than
/// of a value: whether a path resolves on a real store, whether a `set` at it
/// is accepted, whether a subscription pushes, and what happens below an
/// `Option<Struct>` that is `None`. Those need a host process, and they are
/// `live_host_test.dart`.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

// Prefixed because both goldens declare an `Editor` — the same schema type,
// generated into two files, and two distinct Dart types.
import 'generated/app.duet.dart' as app;
import 'generated/wide.duet.dart' as wide;
import 'support/schema_corpus.dart';

/// Every generated codec, by schema fixture and then by schema type.
///
/// Hand-written on purpose: Dart has no reflection under `dart test`, so this
/// is the one place the corpus's type names meet the generated declarations.
/// The assertion that it covers each schema exactly is what keeps it honest —
/// a schema type added and not listed here fails rather than going unchecked.
final Map<String, Map<String, DuetCodec<Object>>> _codecs =
    <String, Map<String, DuetCodec<Object>>>{
  'app': <String, DuetCodec<Object>>{
    'App': const app.AppCodec(),
    'Editor': const app.EditorCodec(),
    'Unlucky': const app.UnluckyCodec(),
  },
  'wide': <String, DuetCodec<Object>>{
    'Editor': const wide.EditorCodec(),
    'Outer': const wide.OuterCodec(),
    'Wide': const wide.WideCodec(),
  },
};

/// The case counts the corpus must contain.
///
/// Pinned as literals for the same reason `wire_corpus_test.dart` pins its
/// own: a corpus test that consumes whatever it is given proves nothing, and a
/// file truncated to one entry would pass every assertion below.
const int _schemaCount = 2;
const int _typeCount = 6;
const int _pathCount = 31;

void main() {
  final SchemaCorpus corpus = SchemaCorpus.load();

  // How many checks actually ran, asserted at the end. Guards the failure
  // where a loop is over an empty list: every assertion inside it passes
  // vacuously and the suite is green having tested nothing.
  int typesChecked = 0;
  int rejectsChecked = 0;
  int pathsChecked = 0;

  group('the corpus itself', () {
    test('is the schema this file reads', () {
      expect(corpus.version, schemaCorpusVersion);
      expect(
        corpus.generator,
        corpusGenerator,
        reason: 'the command that regenerates the file must stay accurate',
      );
    });

    test('holds every schema, type and path this file expects', () {
      expect(corpus.schemas, hasLength(_schemaCount));
      expect(
        corpus.schemas.values
            .map((CorpusSchema s) => s.types.length)
            .fold<int>(0, (int a, int b) => a + b),
        _typeCount,
      );
      expect(
        corpus.schemas.values
            .map((CorpusSchema s) => s.paths.length)
            .fold<int>(0, (int a, int b) => a + b),
        _pathCount,
      );
    });

    test('names exactly the types this package generated code for', () {
      // Both directions. A type in the corpus with no codec here is one
      // nothing checks; a codec here the corpus does not know is one generated
      // from a schema that no longer exists.
      expect(
        <String, Set<String>>{
          for (final CorpusSchema s in corpus.schemas.values)
            s.name: s.types.keys.toSet(),
        },
        <String, Set<String>>{
          for (final MapEntry<String, Map<String, DuetCodec<Object>>> e
              in _codecs.entries)
            e.key: e.value.keys.toSet(),
        },
      );
    });
  });

  for (final CorpusSchema schema in corpus.schemas.values) {
    final Map<String, DuetCodec<Object>> codecs = _codecs[schema.name]!;
    final DuetCodec<Object> root = codecs[schema.root]!;

    group('${schema.name}: the generated codecs', () {
      test('decode the seed the host starts from', () {
        // The value every live-host assertion about an unwritten path is made
        // against. A root codec that could not read it would mean the very
        // first read of a fresh store reported a mismatch.
        expect(
          root.decode(schema.seed),
          isNotNull,
          reason: '${root.name} cannot decode its own schema\'s seed',
        );
      });

      test('re-encode the seed to the exact bytes the corpus states', () {
        // Decode then encode, compared against text Rust produced. A
        // self-inverting round trip cannot see an encoder and a decoder that
        // are wrong in the same direction; this can.
        final Object decoded = root.decode(schema.seed)!;
        expect(root.encode(decoded).toWireText(), schema.seed.toWireText());
      });

      for (final MapEntry<String, List<CorpusField>> entry
          in schema.types.entries) {
        final String name = entry.key;
        final List<CorpusField> fields = entry.value;
        final DuetCodec<Object> codec = codecs[name]!;

        test('$name accepts every field value its schema admits', () {
          expect(
            codec.decode(_filled(fields)),
            isNotNull,
            reason: 'the $name codec refused a value its own schema admits; a '
                'field bound to the wrong codec fails exactly here',
          );
          typesChecked += 1;
        });

        test('$name occupies exactly the wire keys the schema declares', () {
          // The camel-casing check, made against the *encoder*: two guests
          // that disagree about a wire key silently stop seeing each other's
          // writes, and nothing else in this package would notice.
          final DuetValue encoded = codec.encode(codec.decode(_filled(fields))!);
          expect(encoded, isA<DuetMap>());
          expect(
            (encoded as DuetMap).entries.keys.toList()..sort(),
            fields.map((CorpusField f) => f.key).toList()..sort(),
          );
        });

        for (final CorpusField field in fields) {
          test('$name.${field.key} refuses a value of another type', () {
            // One field at a time, every other field left admissible, so a
            // refusal can only be about this one.
            for (final DuetValue reject in field.rejects) {
              final DuetMap probe =
                  _filled(fields, replace: field.key, with_: reject);
              expect(
                codec.decode(probe),
                isNull,
                reason: '${field.key} is a ${field.ty}, and the $name codec '
                    'accepted ${reject.toWireText()}',
              );
              rejectsChecked += 1;
            }
            // `dynamic` is the one type that refuses nothing, and stating that
            // here stops an empty `rejects` list anywhere else passing as
            // "checked".
            expect(
              field.rejects.isEmpty,
              field.ty == 'dynamic',
              reason: 'only a dynamic field may have no rejects',
            );
          });
        }
      }
    });

    group('${schema.name}: the seed, walked by path', () {
      for (final CorpusPath path in schema.paths) {
        test('"${path.path}" holds what the corpus says it holds', () {
          // This package's own path parser and value walker against Rust's,
          // for every path the schema mints. `null` and `DuetNull` are kept
          // apart deliberately: absent and `None` are different states, and
          // the whole `Option` story rests on that.
          final DuetValue? held =
              duetValueAt(schema.seed, DuetPath.parse(path.path));
          expect(held, path.seed, reason: '"${path.path}" is a ${path.ty}');
          pathsChecked += 1;
        });
      }
    });
  }

  tearDownAll(() {
    expect(typesChecked, _typeCount, reason: 'every type must have been decoded');
    expect(rejectsChecked, greaterThan(_pathCount));
    expect(pathsChecked, _pathCount, reason: 'every path must have been walked');
  });
}

/// A map holding every field's admitted value, optionally with one replaced.
DuetMap _filled(
  List<CorpusField> fields, {
  String? replace,
  DuetValue? with_,
}) =>
    DuetMap(<String, DuetValue>{
      for (final CorpusField field in fields)
        field.key: field.key == replace && with_ != null ? with_ : field.accept,
    });
