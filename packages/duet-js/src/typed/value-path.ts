/**
 * Reading and functionally updating a {@link DuetValue} at a {@link DuetPath}.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_value_path.dart`.
 *
 * Both functions here are **iterative**, and both are total: they answer `null`
 * rather than throwing, whatever tree and whatever path they are handed.
 *
 * @module
 */

import type { DuetPath, DuetSegment } from '../path.ts';
import type { DuetValue } from '../value.ts';

/**
 * The value at `path` inside `root`, or `null` when no such node exists.
 *
 * Mirrors `duet_core::Value::get` (crates/duet-core/src/value.rs) exactly,
 * including its collapsing of every failure into one `null`: a missing map key,
 * an out-of-range list index and a segment addressing the wrong kind of node
 * are indistinguishable here, as they are on the host.
 *
 * `null` therefore means "there is no node at this path", which is the same
 * thing {@link DuetClient.get} means by `null` — and is distinct from a node
 * that exists and holds a value of kind `'null'`.
 */
export function duetValueAt(root: DuetValue, path: DuetPath): DuetValue | null {
  let current = root;
  for (const segment of path.segments) {
    const next = childAt(current, segment);
    if (next === null) return null;
    current = next;
  }
  return current;
}

/**
 * `root` with the node at `path` replaced by `value`, or `null` when the write
 * cannot be applied.
 *
 * A new tree is returned; `root` is never mutated, and every container the path
 * does not pass through is shared with `root` rather than copied.
 *
 * The rules are `duet_core::Value::set`'s, so that a mirror updated through
 * this function stays in step with the host that produced the patch:
 *
 * - The root path replaces the whole tree and cannot fail.
 * - Intermediate nodes are never created. Writing to `a.b` when `a` is missing
 *   fails; it does not invent an `a`.
 * - The **final** segment of a map path is inserted if absent, so adding a new
 *   key to an existing map succeeds.
 * - A list index must already be in range. This never appends: an index exactly
 *   equal to the length fails, as it does on the host.
 * - A segment addressing the wrong kind of node — a key against a list, an
 *   index against a map, anything at all against a scalar — fails.
 *
 * # Why one `null` and not a reason
 *
 * `duet_core::Value::set` distinguishes `MissingKey`, `IndexOutOfBounds` and
 * `TypeMismatch`; this function does not. Its one job in this package is the
 * mirror merge in {@link DuetRouter}, where every failure has the same answer —
 * refetch the truth from the host — and the *host* is the authority on why a
 * write was refused, which it states in its `failed` message. A second,
 * client-side taxonomy of write errors would be a parallel truth able to
 * disagree with the one that matters.
 *
 * # Iterative, not recursive
 *
 * The descent and the rebuild are both loops, and neither consumes stack in
 * proportion to `path` or to `root`. That is a hard requirement, not a
 * preference: this function is exported, so any caller may hand it a tree built
 * locally rather than one that arrived over the wire — and a locally built tree
 * has never passed the 127-container check that {@link decodeDuetJson} applies.
 * A recursive rebuild would turn such a call into a `RangeError: Maximum call
 * stack size exceeded`, thrown from outside this package's error type.
 */
export function duetValueWith(
  root: DuetValue,
  path: DuetPath,
  value: DuetValue,
): DuetValue | null {
  const segments = path.segments;
  if (segments.length === 0) return value;

  // Descend, remembering the container each segment indexes into.
  // `containers[i]` is the node `segments[i]` is applied to, so the array ends
  // up exactly as long as `segments`.
  const containers: DuetValue[] = [];
  let current = root;
  for (let i = 0; i < segments.length - 1; i++) {
    containers.push(current);
    const next = childAt(current, segments[i] as DuetSegment);
    // An intermediate node that does not exist is not created — this is the
    // `MissingKey` half of `Value::set`'s contract.
    if (next === null) return null;
    current = next;
  }
  containers.push(current);

  // Rebuild outward from the new leaf, innermost container first.
  let rebuilt = value;
  for (let i = segments.length - 1; i >= 0; i--) {
    const replaced = withChild(
      containers[i] as DuetValue,
      segments[i] as DuetSegment,
      rebuilt,
    );
    if (replaced === null) return null;
    rebuilt = replaced;
  }
  return rebuilt;
}

/** The path `path` addresses relative to its first `from` segments. */
export function duetPathSuffix(path: DuetPath, from: number): DuetPath {
  return { segments: path.segments.slice(from) };
}

/**
 * The child `segment` selects from `node`, or `null` if there is none.
 *
 * The index bound is checked at both ends. {@link parseDuetPath} cannot produce
 * a negative index, but a `DuetPath` is a plain object literal any caller can
 * build, so a hand-built path can carry one — and with
 * `noUncheckedIndexedAccess` off at the call site a bare `items[index]` would
 * hand back `undefined` and be treated as a value.
 */
function childAt(node: DuetValue, segment: DuetSegment): DuetValue | null {
  if (node.kind === 'map' && segment.kind === 'key') {
    return node.entries.get(segment.key) ?? null;
  }
  if (node.kind === 'list' && segment.kind === 'index') {
    if (segment.index < 0 || segment.index >= node.items.length) return null;
    return node.items[segment.index] as DuetValue;
  }
  return null;
}

/**
 * `node` with the child `segment` selects replaced by `child`, or `null` if
 * `segment` cannot address a child of `node`.
 *
 * A map key absent from `node` is **inserted**, which is what makes writing a
 * new key to an existing map succeed; a list index outside the current range is
 * refused, which is what makes writes never append.
 */
function withChild(
  node: DuetValue,
  segment: DuetSegment,
  child: DuetValue,
): DuetValue | null {
  if (node.kind === 'map' && segment.kind === 'key') {
    // `new Map(...)` then `set` keeps an existing key in its original position,
    // which matters only for readability — the encoder sorts on the way out.
    const entries = new Map(node.entries);
    entries.set(segment.key, child);
    return { kind: 'map', entries };
  }
  if (node.kind === 'list' && segment.kind === 'index') {
    if (segment.index < 0 || segment.index >= node.items.length) return null;
    const items = [...node.items];
    items[segment.index] = child;
    return { kind: 'list', items };
  }
  return null;
}
