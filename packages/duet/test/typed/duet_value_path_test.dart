/// `duetValueAt` and `duetValueWith`: the read and the functional update.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

/// A map of maps `depth` levels deep with [leaf] at the bottom, built from the
/// inside out so that constructing the fixture cannot itself overflow.
DuetValue _nest(int depth, DuetValue leaf) {
  DuetValue current = leaf;
  for (int i = 0; i < depth; i++) {
    current = DuetMap(<String, DuetValue>{'k': current});
  }
  return current;
}

/// `k.k.k...`, [depth] segments long.
DuetPath _nestPath(int depth) =>
    DuetPath.parse(List<String>.filled(depth, 'k').join('.'));

DuetValue _tree() => DuetMap(<String, DuetValue>{
      'editor': DuetMap(<String, DuetValue>{
        'zoom': const DuetFloat(1),
        'mode': const DuetStr('select'),
      }),
      'documents': DuetList(<DuetValue>[
        DuetMap(<String, DuetValue>{'title': const DuetStr('one')}),
        DuetMap(<String, DuetValue>{'title': const DuetStr('two')}),
      ]),
      'flag': const DuetBool(true),
    });

void main() {
  group('duetValueAt', () {
    test('the root path is the whole tree', () {
      expect(duetValueAt(_tree(), DuetPath.root), _tree());
    });

    test('reads through maps and lists', () {
      expect(
        duetValueAt(_tree(), DuetPath.parse('editor.zoom')),
        const DuetFloat(1),
      );
      expect(
        duetValueAt(_tree(), DuetPath.parse('documents[1].title')),
        const DuetStr('two'),
      );
    });

    test('a missing key, an out-of-range index and a wrong kind all answer null',
        () {
      expect(duetValueAt(_tree(), DuetPath.parse('editor.nope')), isNull);
      expect(duetValueAt(_tree(), DuetPath.parse('documents[2]')), isNull);
      // A key against a list, an index against a map, and anything at all
      // against a scalar.
      expect(duetValueAt(_tree(), DuetPath.parse('documents.title')), isNull);
      expect(duetValueAt(_tree(), DuetPath.parse('editor[0]')), isNull);
      expect(duetValueAt(_tree(), DuetPath.parse('flag.anything')), isNull);
    });

    test('a hand-built negative index is refused, not thrown at', () {
      // `DuetPath.parse` cannot produce this, but `DuetIndexSegment` is a
      // public unchecked constructor, and a bare `items[index]` would throw a
      // RangeError out of a function documented to be total.
      const DuetPath path = DuetPath(<DuetSegment>[
        DuetKeySegment('documents'),
        DuetIndexSegment(-1),
      ]);
      expect(duetValueAt(_tree(), path), isNull);
    });
  });

  group('duetValueWith', () {
    test('the root path replaces the whole tree and cannot fail', () {
      expect(
        duetValueWith(_tree(), DuetPath.root, const DuetInt(1)),
        const DuetInt(1),
      );
    });

    test('overwrites a leaf without touching its siblings', () {
      final DuetValue? updated = duetValueWith(
        _tree(),
        DuetPath.parse('editor.zoom'),
        const DuetFloat(4),
      );
      expect(duetValueAt(updated!, DuetPath.parse('editor.zoom')),
          const DuetFloat(4));
      expect(duetValueAt(updated, DuetPath.parse('editor.mode')),
          const DuetStr('select'));
      expect(duetValueAt(updated, DuetPath.parse('flag')), const DuetBool(true));
    });

    test('does not mutate the tree it was given', () {
      final DuetValue original = _tree();
      duetValueWith(original, DuetPath.parse('editor.zoom'), const DuetFloat(9));
      expect(original, _tree());
    });

    test('inserts a new key at the final segment of a map path', () {
      final DuetValue? updated = duetValueWith(
        _tree(),
        DuetPath.parse('editor.grid'),
        const DuetBool(false),
      );
      expect(duetValueAt(updated!, DuetPath.parse('editor.grid')),
          const DuetBool(false));
    });

    test('never creates an intermediate node', () {
      // `a.b` when `a` is missing is a refusal on the host, not an implicit
      // insert, and a mirror that invented the `a` would drift from it.
      expect(
        duetValueWith(_tree(), DuetPath.parse('nope.deeper'), const DuetInt(1)),
        isNull,
      );
    });

    test('writes into a list in range and refuses one out of range', () {
      final DuetValue? updated = duetValueWith(
        _tree(),
        DuetPath.parse('documents[0].title'),
        const DuetStr('renamed'),
      );
      expect(duetValueAt(updated!, DuetPath.parse('documents[0].title')),
          const DuetStr('renamed'));
      expect(duetValueAt(updated, DuetPath.parse('documents[1].title')),
          const DuetStr('two'));

      // Exactly at the length: a refusal, never an append.
      expect(
        duetValueWith(
          _tree(),
          DuetPath.parse('documents[2]'),
          const DuetInt(1),
        ),
        isNull,
      );
    });

    test('refuses a segment against the wrong kind of node', () {
      expect(
        duetValueWith(_tree(), DuetPath.parse('documents.title'), const DuetInt(1)),
        isNull,
      );
      expect(
        duetValueWith(_tree(), DuetPath.parse('editor[0]'), const DuetInt(1)),
        isNull,
      );
      expect(
        duetValueWith(_tree(), DuetPath.parse('flag.deeper'), const DuetInt(1)),
        isNull,
      );
    });

    test('a hand-built negative index is refused, not thrown at', () {
      const DuetPath path = DuetPath(<DuetSegment>[
        DuetKeySegment('documents'),
        DuetIndexSegment(-1),
      ]);
      expect(duetValueWith(_tree(), path, const DuetInt(1)), isNull);
    });
  });

  group('depth', () {
    test('the fixture really does sit on the wire limit', () {
      // 63 nested maps encode to 2 containers each, plus 1 for the leaf: 127,
      // exactly `maxJsonDepth`. One more level is 129 and must be refused.
      // Without this assertion the "at the limit" test below would be a test
      // at whatever depth the arithmetic happened to produce.
      final String atLimit = _nest(63, const DuetStr('leaf')).toWireText();
      expect(DuetValue.fromWireText(atLimit), isA<DuetMap>());

      final String overLimit = _nest(64, const DuetStr('leaf')).toWireText();
      expect(
        () => DuetValue.fromWireText(overLimit),
        throwsA(isA<DuetCodecException>()
            .having((DuetCodecException e) => e.reason, 'reason', 'bad_json')),
      );
    });

    test('duetValueWith survives a value at the wire depth limit', () {
      final DuetValue deep = _nest(63, const DuetStr('leaf'));
      final DuetPath path = _nestPath(63);

      final DuetValue? updated = duetValueWith(deep, path, const DuetStr('new'));
      expect(duetValueAt(updated!, path), const DuetStr('new'));
      // And the result is still a legal wire value, so the rebuild did not
      // quietly add a level.
      expect(DuetValue.fromWireText(updated.toWireText()), isA<DuetMap>());
    });

    test('duetValueWith survives a tree far deeper than the wire allows', () {
      // The wire limit is *not* what makes this test meaningful, and saying so
      // is the point: 63 nested frames would not trouble any stack, so the
      // "at the limit" test above cannot tell a recursive rebuild from an
      // iterative one. This one can. `duetValueWith` is public, so a caller
      // may hand it a tree built locally that never passed the wire's depth
      // check.
      //
      // Measured on the Dart VM's default stack: a recursive rebuild of this
      // shape succeeds at 5 000, throws StackOverflowError at 10 000 and
      // above; the iterative one succeeds at every depth up to 100 000.
      //
      // Nothing here compares whole values or renders one: `DuetValue`'s own
      // `==`, `hashCode` and `toString` are recursive, so asserting on the
      // tree as a whole would overflow in the *test* rather than the code.
      const int depth = 100000;
      final DuetValue deep = _nest(depth, const DuetStr('leaf'));
      final DuetPath path = _nestPath(depth);

      expect(duetValueAt(deep, path), const DuetStr('leaf'));

      final DuetValue? updated = duetValueWith(deep, path, const DuetStr('new'));
      expect(updated, isNotNull);
      expect(duetValueAt(updated!, path), const DuetStr('new'));
    });
  });
}
