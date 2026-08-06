/**
 * Paths addressing into the host's state tree.
 *
 * Mirrors `duet_core::Path` (crates/duet-core/src/path.rs) and
 * `packages/duet/lib/src/duet_path.dart`.
 *
 * @module
 */

import { DuetCodecError, DuetReason, echoBounded } from './errors.ts';

/** A string-keyed map lookup, e.g. the `editor` in `editor.zoom`. */
export interface DuetKeySegment {
  readonly kind: 'key';
  /** The key this segment looks up. */
  readonly key: string;
}

/** A list index lookup, e.g. the `3` in `documents[3]`. */
export interface DuetIndexSegment {
  readonly kind: 'index';
  /** The zero-based position this segment looks up. */
  readonly index: number;
}

/** One step in a {@link DuetPath}: either a map key or a list index. */
export type DuetSegment = DuetKeySegment | DuetIndexSegment;

/**
 * An address into the host's state tree. The empty path is the root.
 *
 * Kept as a parsed list of segments rather than an opaque string on purpose: a
 * client that carried the raw string could re-emit any garbage it was handed,
 * which is both a silent way to send the host a path it will reject and — in
 * the golden corpus — a way to "pass" a decode test by echoing the input.
 */
export interface DuetPath {
  /** The steps this path takes, outermost first. */
  readonly segments: readonly DuetSegment[];
}

/** The root path: no segments at all. */
export const DUET_ROOT_PATH: DuetPath = Object.freeze({ segments: Object.freeze([]) });

/**
 * The largest list index this client will parse.
 *
 * Rust bounds an index by `usize` and Dart by its 64-bit `int`; JavaScript
 * stops earlier, because past 2^53−1 a `number` no longer holds consecutive
 * integers. `[9007199254740993]` would parse to `9007199254740992` and render
 * back as a *different path* — so it is refused instead. No real state tree
 * reaches this, and a silently rewritten path is far worse than a rejected one.
 */
export const MAX_DUET_INDEX = Number.MAX_SAFE_INTEGER;

const CHAR_DOT = 0x2e;
const CHAR_LEFT_BRACKET = 0x5b;
const CHAR_RIGHT_BRACKET = 0x5d;
const CHAR_ZERO = 0x30;
const CHAR_NINE = 0x39;

/**
 * Parses a path such as `editor.zoom` or `documents[3].title`.
 *
 * The grammar, mirroring `duet_core::Path::parse` exactly:
 *
 * - A dot-separated sequence of keys, where a key may be followed by one or
 *   more bracketed indices (`documents[3][1]`).
 * - An index may **not** immediately follow a `.`: `a.[0]` is not legal syntax;
 *   write `a[0]`. A leading index with no preceding key (`[0]`) is legal.
 * - The empty string parses to {@link DUET_ROOT_PATH}.
 * - A key is any run of characters other than `.`, `[` or `]`, including
 *   whitespace; keys are not trimmed. `" "`, `"a b"` and `"🦀"` are keys.
 * - An index must be a canonical decimal integer: digits only, no `+` or `-`,
 *   and no leading zero unless the index is exactly `0`. `[007]`, `[+3]` and
 *   `[]` are rejected, so that parsing and {@link formatDuetPath} are mutually
 *   inverse — exactly one spelling per path.
 *
 * # Offsets in the error messages are UTF-16 code units, not bytes
 *
 * `duet_core::PathParseError` carries **byte** offsets into the UTF-8 input.
 * JavaScript strings are UTF-16, so the same character sits at a different
 * index here. The offsets below are JavaScript string indices, which is what a
 * JavaScript caller can actually use; they will not match the host's for any
 * path containing a non-ASCII character.
 *
 * @throws DuetCodecError with `bad_path` if the string does not match the
 *   grammar.
 */
export function parseDuetPath(s: string): DuetPath {
  if (s.length === 0) return DUET_ROOT_PATH;

  const segments: DuetSegment[] = [];
  let i = 0;
  // True immediately after consuming a `.`: the grammar requires a key — not an
  // index — to follow, since `a.[0]` is not legal syntax.
  let expectKey = false;

  while (i < s.length) {
    const scanned =
      s.charCodeAt(i) === CHAR_LEFT_BRACKET ? scanIndex(s, i, expectKey) : scanKey(s, i);
    segments.push(scanned.segment);
    i = scanned.next;
    expectKey = false;

    if (i < s.length && s.charCodeAt(i) === CHAR_DOT) {
      i += 1;
      expectKey = true;
    }
  }

  if (expectKey) {
    throw new DuetCodecError(
      DuetReason.badPath,
      "path ends with a trailing '.' with no key following it",
    );
  }
  return { segments };
}

/**
 * The canonical string spelling, the inverse of {@link parseDuetPath}.
 *
 * A key is joined with `.` to whatever precedes it; an index is appended in
 * brackets with no separator, which is why `a[0]` and not `a.[0]` is the legal
 * spelling.
 *
 * Renders each key verbatim, so the result round-trips through
 * {@link parseDuetPath} only if every key is non-empty and avoids `.`, `[` and
 * `]`. Every path {@link parseDuetPath} produces satisfies that; a path built
 * by hand from untrusted data must be run through the parser first.
 */
export function formatDuetPath(path: DuetPath): string {
  let out = '';
  let first = true;
  for (const segment of path.segments) {
    if (segment.kind === 'key') {
      if (!first) out += '.';
      out += segment.key;
    } else {
      out += `[${String(segment.index)}]`;
    }
    first = false;
  }
  return out;
}

/** True when `path` addresses `other` or any ancestor of it. */
export function duetPathIsPrefixOf(path: DuetPath, other: DuetPath): boolean {
  if (path.segments.length > other.segments.length) return false;
  return path.segments.every((segment, i) =>
    duetSegmentEquals(segment, other.segments[i] as DuetSegment),
  );
}

/** Structural equality on one path step. */
export function duetSegmentEquals(a: DuetSegment, b: DuetSegment): boolean {
  if (a.kind === 'key') return b.kind === 'key' && a.key === b.key;
  return b.kind === 'index' && a.index === b.index;
}

/** Structural equality on a whole path. */
export function duetPathEquals(a: DuetPath, b: DuetPath): boolean {
  return (
    a.segments.length === b.segments.length &&
    a.segments.every((segment, i) => duetSegmentEquals(segment, b.segments[i] as DuetSegment))
  );
}

/**
 * Scans a bracketed index starting at the `[` at `at`. Returns the segment and
 * the index just past the closing `]`, after checking that whatever follows the
 * bracket is legal.
 */
function scanIndex(s: string, at: number, expectKey: boolean): { segment: DuetSegment; next: number } {
  if (expectKey) {
    throw new DuetCodecError(DuetReason.badPath, `unexpected '[' at offset ${String(at)}`);
  }
  const start = at + 1;
  const end = s.indexOf(']', start);
  if (end < 0) {
    throw new DuetCodecError(DuetReason.badPath, `unclosed '[' at offset ${String(at)}`);
  }
  const raw = s.slice(start, end);
  const index = canonicalIndex(raw);
  if (index === null) {
    throw new DuetCodecError(
      DuetReason.badPath,
      `invalid index ${echoBounded(raw)} in brackets opened at offset ${String(at)}: ` +
        'expected a canonical non-negative decimal integer',
    );
  }

  const next = end + 1;
  // After a closing `]`, only `.`, `[` or end-of-input may follow.
  if (
    next < s.length &&
    s.charCodeAt(next) !== CHAR_DOT &&
    s.charCodeAt(next) !== CHAR_LEFT_BRACKET
  ) {
    throw new DuetCodecError(
      DuetReason.badPath,
      `unexpected character ${echoBounded(s[next] as string)} at offset ${String(next)}`,
    );
  }
  return { segment: { kind: 'index', index }, next };
}

/**
 * Parses the text between brackets, or `null` if it is not a canonical
 * non-negative decimal integer this client can represent exactly.
 */
function canonicalIndex(raw: string): number | null {
  if (raw.length === 0) return null;
  for (let i = 0; i < raw.length; i++) {
    const c = raw.charCodeAt(i);
    if (c < CHAR_ZERO || c > CHAR_NINE) return null;
  }
  if (raw.length > 1 && raw.startsWith('0')) return null;
  // `Number` on a long digit string rounds rather than failing, so the result
  // is range-checked instead of trusted. Any 16-digit-or-shorter string that
  // rounds must round to at least 2^53, which is already above
  // MAX_DUET_INDEX — so the two checks together admit only exactly
  // representable indices. Longer strings are refused without conversion, to
  // keep the work on untrusted input O(1) past that point.
  if (raw.length > 16) return null;
  const parsed = Number(raw);
  return parsed > MAX_DUET_INDEX ? null : parsed;
}

/**
 * Scans a key starting at `at`: any run of characters other than `.`, `[` or
 * `]`. Returns the segment and the index just past the last character taken.
 */
function scanKey(s: string, at: number): { segment: DuetSegment; next: number } {
  let end = at;
  while (end < s.length) {
    const c = s.charCodeAt(end);
    if (c === CHAR_DOT || c === CHAR_LEFT_BRACKET || c === CHAR_RIGHT_BRACKET) break;
    end += 1;
  }
  if (end < s.length && s.charCodeAt(end) === CHAR_RIGHT_BRACKET) {
    throw new DuetCodecError(
      DuetReason.badPath,
      `unexpected character ']' at offset ${String(end)}`,
    );
  }
  if (end === at) {
    throw new DuetCodecError(
      DuetReason.badPath,
      `expected a key at offset ${String(at)}, found none`,
    );
  }
  return { segment: { kind: 'key', key: s.slice(at, end) }, next: end };
}
