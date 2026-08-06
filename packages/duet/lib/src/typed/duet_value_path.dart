/// Reading and functionally updating a [DuetValue] at a [DuetPath].
///
/// Both functions here are **iterative**, and both are total: they answer
/// `null` rather than throwing, whatever tree and whatever path they are
/// handed.
library;

import '../duet_path.dart';
import '../duet_value.dart';

/// The value at [path] inside [root], or `null` when no such node exists.
///
/// Mirrors `duet_core::Value::get` (crates/duet-core/src/value.rs) exactly,
/// including its collapsing of every failure into one `null`: a missing map
/// key, an out-of-range list index and a segment addressing the wrong kind of
/// node are indistinguishable here, as they are on the host.
///
/// `null` therefore means "there is no node at this path", which is the same
/// thing `DuetClient.get` means by `null` — and is distinct from a node that
/// exists and holds [DuetNull].
DuetValue? duetValueAt(DuetValue root, DuetPath path) {
  DuetValue current = root;
  for (final DuetSegment segment in path.segments) {
    final DuetValue? next = _childAt(current, segment);
    if (next == null) return null;
    current = next;
  }
  return current;
}

/// [root] with the node at [path] replaced by [value], or `null` when the
/// write cannot be applied.
///
/// A new tree is returned; [root] is never mutated, and every container the
/// path does not pass through is shared with [root] rather than copied.
///
/// The rules are `duet_core::Value::set`'s, so that a mirror updated through
/// this function stays in step with the host that produced the patch:
///
/// - The root path replaces the whole tree and cannot fail.
/// - Intermediate nodes are never created. Writing to `a.b` when `a` is
///   missing fails; it does not invent an `a`.
/// - The **final** segment of a map path is inserted if absent, so adding a
///   new key to an existing map succeeds.
/// - A list index must already be in range. This never appends: an index
///   exactly equal to the length fails, as it does on the host.
/// - A segment addressing the wrong kind of node — a key against a list, an
///   index against a map, anything at all against a scalar — fails.
///
/// # Why one `null` and not a reason
///
/// `duet_core::Value::set` distinguishes `MissingKey`, `IndexOutOfBounds` and
/// `TypeMismatch`; this function does not. Its one job in this package is the
/// mirror merge in [DuetRouter], where every failure has the same answer —
/// refetch the truth from the host — and the *host* is the authority on why a
/// write was refused, which it states in its `failed` message. A second,
/// client-side taxonomy of write errors would be a parallel truth able to
/// disagree with the one that matters. Its one job in this package is the
/// mirror merge in `DuetRouter`.
///
/// # Iterative, not recursive
///
/// The descent and the rebuild are both loops, and neither consumes stack in
/// proportion to [path] or to [root]. That is a hard requirement, not a
/// preference: this function is public, so any caller may hand it a tree built
/// locally rather than one that arrived over the wire — and a locally built
/// tree has never passed the 127-container check that `decodeDuetJson`
/// applies. A recursive rebuild would turn such a call into a stack overflow,
/// which in Dart is not catchable and takes the isolate with it.
DuetValue? duetValueWith(DuetValue root, DuetPath path, DuetValue value) {
  final List<DuetSegment> segments = path.segments;
  if (segments.isEmpty) return value;

  // Descend, remembering the container each segment indexes into.
  // `containers[i]` is the node `segments[i]` is applied to, so the list ends
  // up exactly as long as `segments`.
  final List<DuetValue> containers = <DuetValue>[];
  DuetValue current = root;
  for (int i = 0; i < segments.length - 1; i++) {
    containers.add(current);
    final DuetValue? next = _childAt(current, segments[i]);
    // An intermediate node that does not exist is not created — this is the
    // `MissingKey` half of `Value::set`'s contract.
    if (next == null) return null;
    current = next;
  }
  containers.add(current);

  // Rebuild outward from the new leaf, innermost container first.
  DuetValue rebuilt = value;
  for (int i = segments.length - 1; i >= 0; i--) {
    final DuetValue? replaced = _withChild(containers[i], segments[i], rebuilt);
    if (replaced == null) return null;
    rebuilt = replaced;
  }
  return rebuilt;
}

/// The child [segment] selects from [node], or `null` if there is none.
///
/// The index bound is checked at both ends. `DuetPath.parse` cannot produce a
/// negative index, but [DuetIndexSegment] is a public constructor that
/// performs no validation, so a hand-built path can carry one — and a bare
/// `items[index]` would then throw a `RangeError` out of a function documented
/// to be total.
DuetValue? _childAt(DuetValue node, DuetSegment segment) =>
    switch ((node, segment)) {
      (DuetMap(:final Map<String, DuetValue> entries), DuetKeySegment(:final String key)) =>
        entries[key],
      (DuetList(:final List<DuetValue> items), DuetIndexSegment(:final int index))
          when index >= 0 && index < items.length =>
        items[index],
      _ => null,
    };

/// [node] with the child [segment] selects replaced by [child], or `null` if
/// [segment] cannot address a child of [node].
///
/// A map key absent from [node] is **inserted**, which is what makes writing a
/// new key to an existing map succeed; a list index outside the current range
/// is refused, which is what makes writes never append.
DuetValue? _withChild(DuetValue node, DuetSegment segment, DuetValue child) =>
    switch ((node, segment)) {
      (DuetMap(:final Map<String, DuetValue> entries), DuetKeySegment(:final String key)) =>
        DuetMap(<String, DuetValue>{...entries, key: child}),
      (DuetList(:final List<DuetValue> items), DuetIndexSegment(:final int index))
          when index >= 0 && index < items.length =>
        DuetList(<DuetValue>[
          for (int i = 0; i < items.length; i++)
            if (i == index) child else items[i],
        ]),
      _ => null,
    };
