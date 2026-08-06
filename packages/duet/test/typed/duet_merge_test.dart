/// `duetMergeMirror`: the three-case merge, tested as a pure function.
///
/// Every test here names the wrong implementation it rules out. That is the
/// discipline this file exists for: a merge test that only ever writes to the
/// watched path itself passes against an implementation that ignores the
/// patch's path entirely, which is the most natural way to get this wrong.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

DuetPath _p(String s) => DuetPath.parse(s);

void main() {
  group('case 1 — the patch is at the watched path', () {
    test('the patch is the whole answer', () {
      expect(
        duetMergeMirror(
          watched: _p('editor.zoom'),
          mirror: const DuetFloat(1),
          changed: _p('editor.zoom'),
          value: const DuetFloat(2),
        ),
        const DuetMerged(DuetFloat(2)),
      );
    });

    test('it replaces the mirror even when the value did not change', () {
      // `Store::set` always notifies and never diffs, so a no-op write is a
      // real notification. Nothing here may assume otherwise.
      expect(
        duetMergeMirror(
          watched: _p('a'),
          mirror: const DuetInt(1),
          changed: _p('a'),
          value: const DuetInt(1),
        ),
        const DuetMerged(DuetInt(1)),
      );
    });

    test('the root watcher of a root write takes the whole tree', () {
      expect(
        duetMergeMirror(
          watched: DuetPath.root,
          mirror: null,
          changed: DuetPath.root,
          value: const DuetInt(7),
        ),
        const DuetMerged(DuetInt(7)),
      );
    });
  });

  group('case 2 — the patch is below the watched path', () {
    test('only the patched leaf changes; the siblings survive', () {
      // Rules out `mirror = value`, which would answer Merged(Float(2)) —
      // the leaf standing in for the whole struct.
      final DuetMerge merge = duetMergeMirror(
        watched: _p('editor'),
        mirror: DuetMap(<String, DuetValue>{
          'zoom': const DuetFloat(1),
          'mode': const DuetStr('select'),
        }),
        changed: _p('editor.zoom'),
        value: const DuetFloat(2),
      );
      expect(
        merge,
        DuetMerged(DuetMap(<String, DuetValue>{
          'zoom': const DuetFloat(2),
          'mode': const DuetStr('select'),
        })),
      );
    });

    test('the relative path is used, not the absolute one', () {
      // Rules out `duetValueWith(mirror, changed, value)`. That spelling walks
      // `editor` inside the *mirror of* `editor`, finds nothing, and answers
      // Resync — so this assertion is the one that separates them.
      final DuetMerge merge = duetMergeMirror(
        watched: _p('editor'),
        mirror: DuetMap(<String, DuetValue>{'zoom': const DuetFloat(1)}),
        changed: _p('editor.zoom'),
        value: const DuetFloat(5),
      );
      expect(
        merge,
        DuetMerged(DuetMap(<String, DuetValue>{'zoom': const DuetFloat(5)})),
      );
    });

    test('a patch two levels below still lands in the right place', () {
      final DuetMerge merge = duetMergeMirror(
        watched: _p('a'),
        mirror: DuetMap(<String, DuetValue>{
          'b': DuetMap(<String, DuetValue>{'c': const DuetInt(1)}),
        }),
        changed: _p('a.b.c'),
        value: const DuetInt(2),
      );
      expect(
        merge,
        DuetMerged(DuetMap(<String, DuetValue>{
          'b': DuetMap(<String, DuetValue>{'c': const DuetInt(2)}),
        })),
      );
    });

    test('an absent mirror resyncs rather than inventing the structure', () {
      // The null-mirror refetch. Reachable: the host refuses a write below an
      // absent node, so a below-patch arriving against an absent mirror means
      // the mirror is stale, not that the host is wrong.
      expect(
        duetMergeMirror(
          watched: _p('editor'),
          mirror: null,
          changed: _p('editor.zoom'),
          value: const DuetFloat(2),
        ),
        isA<DuetResync>().having(
          (DuetResync r) => r.reason,
          'reason',
          contains('no mirror'),
        ),
      );
    });

    test('a mirror with no room for the patched path resyncs', () {
      // The mirror says `editor` is a scalar; the host says it has a `zoom`.
      // One of the two is stale and it is not the host.
      expect(
        duetMergeMirror(
          watched: _p('editor'),
          mirror: const DuetStr('stale'),
          changed: _p('editor.zoom'),
          value: const DuetFloat(2),
        ),
        isA<DuetResync>().having(
          (DuetResync r) => r.reason,
          'reason',
          contains('no room'),
        ),
      );
    });
  });

  group('case 3 — the patch is above the watched path', () {
    test('the watched node is read out of the ancestor at the relative path',
        () {
      // The single most important assertion in this file, and the reason the
      // ancestor here is `a.b` and not the root.
      //
      // Rules out two wrong implementations at once:
      //   - `mirror = value` answers Merged(Map{c: 5}) — the ancestor standing
      //     in for the leaf.
      //   - `duetValueAt(value, watched)` — the *absolute* path — walks `a.b.c`
      //     inside a value that is already `a.b`, finds nothing, and answers
      //     Merged(null).
      //
      // Both of those agree with the correct answer when the ancestor is the
      // root path, which is why a root-only ancestor test cannot catch either.
      final DuetMerge merge = duetMergeMirror(
        watched: _p('a.b.c'),
        mirror: const DuetInt(1),
        changed: _p('a.b'),
        value: DuetMap(<String, DuetValue>{'c': const DuetInt(5)}),
      );
      expect(merge, const DuetMerged(DuetInt(5)));
    });

    test('a root write is resolved down to the watched leaf', () {
      final DuetMerge merge = duetMergeMirror(
        watched: _p('editor.zoom'),
        mirror: const DuetFloat(1),
        changed: DuetPath.root,
        value: DuetMap(<String, DuetValue>{
          'editor': DuetMap(<String, DuetValue>{'zoom': const DuetFloat(3)}),
        }),
      );
      expect(merge, const DuetMerged(DuetFloat(3)));
    });

    test('an ancestor that no longer contains the path makes it absent, and '
        'does not resync', () {
      // `null` here is a fact, not a gap: the ancestor's value is complete and
      // authoritative for everything under it. Rules out an implementation
      // that treats "not found while descending" as a reason to refetch, which
      // would put a round trip on every `Option` going to `None`.
      expect(
        duetMergeMirror(
          watched: _p('editor.zoom'),
          mirror: const DuetFloat(1),
          changed: _p('editor'),
          value: const DuetNull(),
        ),
        const DuetMerged(null),
      );
    });

    test('an ancestor replaced by a scalar also makes the path absent', () {
      expect(
        duetMergeMirror(
          watched: _p('a.b.c'),
          mirror: const DuetInt(1),
          changed: _p('a.b'),
          value: const DuetStr('scalar'),
        ),
        const DuetMerged(null),
      );
    });

    test('a list ancestor resolves an indexed watch', () {
      expect(
        duetMergeMirror(
          watched: _p('documents[1].title'),
          mirror: const DuetStr('two'),
          changed: _p('documents'),
          value: DuetList(<DuetValue>[
            DuetMap(<String, DuetValue>{'title': const DuetStr('a')}),
            DuetMap(<String, DuetValue>{'title': const DuetStr('b')}),
          ]),
        ),
        const DuetMerged(DuetStr('b')),
      );
    });
  });

  group('paths that overlap in neither direction', () {
    // A conforming host never sends one — `Store::set` only notifies
    // subscriptions whose path overlaps the write. Reaching one means this
    // router and the host disagree about what the subscription watches, and
    // the only answer that converges is to ask.
    test('same length, different path', () {
      expect(
        duetMergeMirror(
          watched: _p('a.b'),
          mirror: const DuetInt(1),
          changed: _p('a.c'),
          value: const DuetInt(2),
        ),
        isA<DuetResync>(),
      );
    });

    test('longer, but not inside the watched subtree', () {
      expect(
        duetMergeMirror(
          watched: _p('a.b'),
          mirror: const DuetInt(1),
          changed: _p('x.y.z'),
          value: const DuetInt(2),
        ),
        isA<DuetResync>(),
      );
    });

    test('shorter, but not an ancestor', () {
      expect(
        duetMergeMirror(
          watched: _p('a.b.c'),
          mirror: const DuetInt(1),
          changed: _p('x'),
          value: const DuetInt(2),
        ),
        isA<DuetResync>(),
      );
    });

    test('a key and an index at the same position do not overlap', () {
      expect(
        duetMergeMirror(
          watched: _p('a.b'),
          mirror: const DuetInt(1),
          changed: _p('a[0]'),
          value: const DuetInt(2),
        ),
        isA<DuetResync>(),
      );
    });
  });
}
