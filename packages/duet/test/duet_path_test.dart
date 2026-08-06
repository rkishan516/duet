import 'package:duet/duet.dart';
import 'package:test/test.dart';

void main() {
  test('parse and toString are mutually inverse', () {
    // duet-core proves this by exhaustive property test for the Rust side
    // (crates/duet-core/src/path.rs). The wire relies on it: `encode_path`
    // ships `Display`'s output and every decoder re-parses it, so a path that
    // did not round-trip would arrive as a *different* path.
    for (final String canonical in <String>[
      '',
      'a',
      'a.b',
      'editor.zoom',
      'documents[3].title',
      '[0]',
      'a[0][1]',
      'a[0].b',
      'a b',
      ' ',
      '🦀',
      'a.🦀[10]',
    ]) {
      final DuetPath path = DuetPath.parse(canonical);
      expect(path.toString(), canonical, reason: canonical);
      expect(DuetPath.parse(path.toString()), path, reason: canonical);
    }
  });

  test('the empty path is the root', () {
    expect(DuetPath.parse('').isRoot, isTrue);
    expect(DuetPath.root.segments, isEmpty);
    expect(DuetPath.parse('a').isRoot, isFalse);
  });

  test('segments carry the parsed structure, not the raw text', () {
    // The point of parsing at all: a client that kept the string could re-emit
    // any garbage it was handed, and would "pass" a decode test by echoing its
    // input.
    expect(DuetPath.parse('documents[3].title').segments, <DuetSegment>[
      const DuetKeySegment('documents'),
      const DuetIndexSegment(3),
      const DuetKeySegment('title'),
    ]);
  });

  test('an index may not follow a dot', () {
    // `a.[0]` is the corpus reject case `envelope/path/unparseable`. The
    // grammar has exactly one spelling for that path, and it is `a[0]`.
    expect(DuetPath.parse('a[0]').segments.length, 2);
    expect(
      () => DuetPath.parse('a.[0]'),
      throwsA(
        isA<DuetCodecException>().having(
          (DuetCodecException e) => e.reason,
          'reason',
          DuetReason.badPath,
        ),
      ),
    );
  });

  test('every malformed path is refused with reason bad_path', () {
    // Each entry names the rule it breaks, mirroring duet-core's
    // PathParseError variants.
    const Map<String, String> bad = <String, String>{
      '.a': 'a leading dot leaves an empty first segment',
      'a..b': 'a doubled dot leaves an empty segment',
      'a.': 'a trailing dot has no key after it',
      'a[': 'an unclosed bracket',
      'a[]': 'an empty index',
      'a[007]': 'a non-canonical index',
      'a[+3]': 'a signed index',
      'a[-1]': 'a negative index',
      'a[x]': 'a non-numeric index',
      'a]': 'a stray closing bracket',
      'a[0]b': 'text directly after a closing bracket',
      '[0]]': 'a stray closing bracket after an index',
      'a[99999999999999999999]': 'an index past the integer range',
    };
    bad.forEach((String path, String why) {
      expect(
        () => DuetPath.parse(path),
        throwsA(
          isA<DuetCodecException>().having(
            (DuetCodecException e) => e.reason,
            'reason',
            DuetReason.badPath,
          ),
        ),
        reason: '$path: $why',
      );
    });
  });

  test('a key is any run of characters other than . [ ]', () {
    // Keys are not trimmed and are not restricted to identifiers — mirrors
    // duet-core's documented grammar.
    expect(DuetPath.parse('a b').segments, <DuetSegment>[
      const DuetKeySegment('a b'),
    ]);
    expect(DuetPath.parse('\t').segments, <DuetSegment>[
      const DuetKeySegment('\t'),
    ]);
  });

  test('prefix matching is the subscription rule', () {
    // A subscriber at `a` must see writes at `a.b`; a subscriber at `a.b` must
    // see writes at `a`, since an ancestor write changes its value too.
    expect(DuetPath.parse('a').isPrefixOf(DuetPath.parse('a.b')), isTrue);
    expect(DuetPath.parse('a.b').isPrefixOf(DuetPath.parse('a')), isFalse);
    expect(DuetPath.root.isPrefixOf(DuetPath.parse('a[0].b')), isTrue);
    expect(DuetPath.parse('a').isPrefixOf(DuetPath.parse('ab')), isFalse);
  });

  test('equal paths are equal values, whatever built them', () {
    expect(
        DuetPath.parse('a[0]'),
        const DuetPath(<DuetSegment>[
          DuetKeySegment('a'),
          DuetIndexSegment(0),
        ]));
    expect(
      DuetPath.parse('a[0]').hashCode,
      const DuetPath(<DuetSegment>[
        DuetKeySegment('a'),
        DuetIndexSegment(0),
      ]).hashCode,
    );
    expect(DuetPath.parse('a'), isNot(DuetPath.parse('b')));
    expect(DuetPath.parse('a'), isNot(DuetPath.parse('a.b')));
    // A key segment and an index segment never compare equal, even when they
    // render similarly.
    expect(const DuetKeySegment('0'), isNot(const DuetIndexSegment(0)));
  });
}
