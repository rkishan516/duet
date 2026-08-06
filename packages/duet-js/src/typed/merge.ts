/**
 * The three-case mirror merge: folding one host patch into one watcher's local
 * copy of one path.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_merge.dart`.
 *
 * @module
 */

import { duetPathEquals, duetPathIsPrefixOf, type DuetPath } from '../path.ts';
import type { DuetValue } from '../value.ts';
import { duetPathSuffix, duetValueAt, duetValueWith } from './value-path.ts';

/** The merge succeeded: the watched path now holds `mirror`. */
export interface DuetMerged {
  readonly kind: 'merged';
  /**
   * The watched path's value after the patch. `null` means the watched path
   * holds no node at all — the same `null` {@link DuetClient.get} uses,
   * distinct from a value of kind `'null'`.
   */
  readonly mirror: DuetValue | null;
}

/**
 * The merge could not be computed locally; the host must be asked.
 *
 * Never an error: it is the correct outcome whenever the patch carries strictly
 * less information than the mirror needs, and the only sound response is a
 * fresh read.
 */
export interface DuetResync {
  readonly kind: 'resync';
  /**
   * A developer-facing explanation, for logging and for tests that want to
   * assert *which* resync path fired rather than merely that one did.
   */
  readonly reason: string;
}

/**
 * What folding a patch into a mirror produced.
 *
 * A tagged pair rather than a nullable value, because "the mirror is now
 * absent" and "the mirror cannot be computed from here" are different answers
 * with different responses, and a `DuetValue | null` return would spell them the
 * same way.
 */
export type DuetMerge = DuetMerged | DuetResync;

/** Builds a successful merge. */
export function duetMerged(mirror: DuetValue | null): DuetMerged {
  return { kind: 'merged', mirror };
}

/** Builds a merge that needs the host's help. */
export function duetResync(reason: string): DuetResync {
  return { kind: 'resync', reason };
}

/**
 * Folds the patch `(changed, value)` into the mirror of `watched`.
 *
 * A pure function of its arguments, deliberately: this is the single hardest
 * piece of logic in the typed runtime, and keeping it free of any client,
 * transport or subscription state is what lets it be tested — and mutated —
 * directly, rather than only through a router that could mask a wrong answer
 * behind a refetch.
 *
 * # Why three cases, and why the wrong one looks right
 *
 * `Store::set` sends **the path that was written**, never the receiving
 * subscriber's own path (crates/duet-core/src/store.rs's `Patch`). A subscriber
 * watching `editor` is notified with `editor.zoom` when that leaf changes, and
 * with the root path when the whole tree is replaced. So `changed` may sit *at*,
 * *below* or *above* `watched`, and all three arrive through the same callback:
 *
 * - **At** — `changed` equals `watched`. The patch is the whole answer.
 * - **Below** — `watched` is a strict prefix of `changed`. Only part of the
 *   mirror changed, at the relative path `changed - watched`.
 * - **Above** — `changed` is a strict prefix of `watched`. The patch carries an
 *   ancestor's new value, and the watched node is whatever now sits at
 *   `watched - changed` *inside* it — which may be nothing.
 *
 * The implementation that looks right and is wrong assigns `value` to the
 * mirror unconditionally. That is correct for *at*, obviously wrong for
 * *below*, and quietly wrong for *above*: the mirror silently becomes the
 * ancestor's value.
 *
 * A second, subtler wrong implementation indexes the *absolute* path into
 * `value` in the above case — `duetValueAt(value, watched)` instead of
 * `duetValueAt(value, watched - changed)`. Those two agree exactly when
 * `changed` is the root path, so a test suite whose only ancestor case is a root
 * write cannot tell them apart. `test/typed/merge.test.ts` therefore uses a
 * non-root ancestor.
 */
export function duetMergeMirror(
  watched: DuetPath,
  mirror: DuetValue | null,
  changed: DuetPath,
  value: DuetValue,
): DuetMerge {
  const watchedLength = watched.segments.length;
  const changedLength = changed.segments.length;

  // Case 1 — at.
  if (changedLength === watchedLength) {
    if (duetPathEquals(watched, changed)) return duetMerged(value);
    return duetResync(
      'the patch names a path of the same length that is not the watched one',
    );
  }

  // Case 2 — below: the patch changed something inside the watched subtree.
  if (changedLength > watchedLength) {
    if (!duetPathIsPrefixOf(watched, changed)) {
      return duetResync('the patch names a path outside the watched subtree');
    }
    // The mirror is where the surrounding structure lives; the patch carries
    // only a leaf. With no mirror there is nothing to write the leaf *into*, and
    // inventing the containers around it would fabricate a shape the host may
    // not have. (This is reachable: the host refuses a write below an absent
    // node, so an arriving below-patch against an absent mirror means the mirror
    // is stale, not that the host is wrong.)
    if (mirror === null) {
      return duetResync(
        'a patch below the watched path arrived with no mirror to fold it into',
      );
    }
    const merged = duetValueWith(mirror, duetPathSuffix(changed, watchedLength), value);
    if (merged === null) {
      return duetResync('the mirror has no room for the patched path');
    }
    return duetMerged(merged);
  }

  // Case 3 — above: the patch replaced an ancestor of the watched path.
  if (!duetPathIsPrefixOf(changed, watched)) {
    return duetResync('the patch names a path that does not enclose the watched one');
  }
  // `value` is the ancestor's complete new value, so it is authoritative for the
  // whole subtree under it — including the answer "the watched node is no longer
  // there". No refetch: a `null` here is a fact, not a gap.
  return duetMerged(duetValueAt(value, duetPathSuffix(watched, changedLength)));
}
