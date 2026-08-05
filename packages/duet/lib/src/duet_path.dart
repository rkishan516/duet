/// Paths addressing into the host's state tree.
library;

import 'duet_error.dart';

/// One step in a [DuetPath]: either a map key or a list index.
///
/// Mirrors `duet_core::Segment` (crates/duet-core/src/path.rs).
sealed class DuetSegment {
  const DuetSegment();
}

/// A string-keyed map lookup, e.g. the `editor` in `editor.zoom`.
final class DuetKeySegment extends DuetSegment {
  /// Creates a key segment. Round-trips through [DuetPath.parse] only if
  /// [key] is non-empty and contains none of `.`, `[`, `]`; [DuetPath.parse]
  /// guarantees that for every path it produces.
  const DuetKeySegment(this.key);

  /// The key this segment looks up.
  final String key;

  @override
  bool operator ==(Object other) => other is DuetKeySegment && other.key == key;

  @override
  int get hashCode => key.hashCode;

  @override
  String toString() => 'Key($key)';
}

/// A list index lookup, e.g. the `3` in `documents[3]`.
final class DuetIndexSegment extends DuetSegment {
  /// Creates an index segment addressing position [index].
  const DuetIndexSegment(this.index);

  /// The zero-based position this segment looks up.
  final int index;

  @override
  bool operator ==(Object other) =>
      other is DuetIndexSegment && other.index == index;

  @override
  int get hashCode => index.hashCode;

  @override
  String toString() => 'Index($index)';
}

/// An address into the host's state tree. The empty path is the root.
///
/// Mirrors `duet_core::Path` (crates/duet-core/src/path.rs). Kept as a parsed
/// list of segments rather than an opaque string on purpose: a client that
/// carried the raw string could re-emit any garbage it was handed, which is
/// both a silent way to send the host a path it will reject and — in the
/// golden corpus — a way to "pass" a decode test by echoing the input.
final class DuetPath {
  /// Wraps an already-validated list of [segments].
  ///
  /// Performs no validation: [toString] renders each [DuetKeySegment]
  /// verbatim, so the result round-trips through [parse] only if every key is
  /// non-empty and avoids `.`, `[` and `]`. Any path built from data arriving
  /// over IPC must go through [parse], which validates unconditionally.
  const DuetPath(this.segments);

  /// The root path: no segments at all.
  static const DuetPath root = DuetPath(<DuetSegment>[]);

  /// The steps this path takes, outermost first.
  final List<DuetSegment> segments;

  /// True when this path addresses the root.
  bool get isRoot => segments.isEmpty;

  /// Parses a path such as `editor.zoom` or `documents[3].title`.
  ///
  /// The grammar, mirroring `duet_core::Path::parse` exactly:
  ///
  /// - A dot-separated sequence of keys, where a key may be followed by one or
  ///   more bracketed indices (`documents[3][1]`).
  /// - An index may **not** immediately follow a `.`: `a.[0]` is not legal
  ///   syntax; write `a[0]`. A leading index with no preceding key (`[0]`) is
  ///   legal.
  /// - The empty string parses to [root].
  /// - A key is any run of characters other than `.`, `[` or `]`, including
  ///   whitespace; keys are not trimmed. `" "`, `"a b"` and `"🦀"` are keys.
  /// - An index must be a canonical decimal integer: digits only, no `+` or
  ///   `-`, and no leading zero unless the index is exactly `0`. `[007]`,
  ///   `[+3]` and `[]` are rejected, so that [parse] and [toString] are
  ///   mutually inverse — exactly one spelling per path.
  ///
  /// # Offsets in the error messages are UTF-16 code units, not bytes
  ///
  /// `duet_core::PathParseError` carries **byte** offsets into the UTF-8
  /// input. Dart strings are UTF-16, so the same character sits at a different
  /// index here. The offsets below are Dart string indices, which is what a
  /// Dart caller can actually use; they will not match the host's for any path
  /// containing a non-ASCII character.
  ///
  /// Throws [DuetCodecException] with reason [DuetReason.badPath] if the
  /// string does not match the grammar.
  static DuetPath parse(String s) {
    if (s.isEmpty) return root;

    final List<DuetSegment> segments = <DuetSegment>[];
    int i = 0;
    // True immediately after consuming a `.`: the grammar requires a key —
    // not an index — to follow, since `a.[0]` is not legal syntax.
    bool expectKey = false;

    while (i < s.length) {
      final (DuetSegment segment, int next) = s.codeUnitAt(i) == _leftBracket
          ? _scanIndex(s, i, expectKey)
          : _scanKey(s, i);
      segments.add(segment);
      i = next;
      expectKey = false;

      if (i < s.length && s.codeUnitAt(i) == _dot) {
        i += 1;
        expectKey = true;
      }
    }

    if (expectKey) {
      throw DuetCodecException.badPath(
        "path ends with a trailing '.' with no key following it",
      );
    }
    return DuetPath(List<DuetSegment>.unmodifiable(segments));
  }

  /// True when this path addresses this path or any ancestor of [other].
  bool isPrefixOf(DuetPath other) {
    if (segments.length > other.segments.length) return false;
    for (int i = 0; i < segments.length; i++) {
      if (segments[i] != other.segments[i]) return false;
    }
    return true;
  }

  /// The canonical string spelling, the inverse of [parse].
  ///
  /// A key is joined with `.` to whatever precedes it; an index is appended in
  /// brackets with no separator, which is why `a[0]` and not `a.[0]` is the
  /// legal spelling.
  @override
  String toString() {
    final StringBuffer out = StringBuffer();
    bool first = true;
    for (final DuetSegment segment in segments) {
      switch (segment) {
        case DuetKeySegment(:final String key):
          if (!first) out.write('.');
          out.write(key);
        case DuetIndexSegment(:final int index):
          out.write('[$index]');
      }
      first = false;
    }
    return out.toString();
  }

  @override
  bool operator ==(Object other) {
    if (other is! DuetPath) return false;
    if (other.segments.length != segments.length) return false;
    for (int i = 0; i < segments.length; i++) {
      if (other.segments[i] != segments[i]) return false;
    }
    return true;
  }

  @override
  int get hashCode => Object.hashAll(segments);
}

const int _dot = 0x2e;
const int _leftBracket = 0x5b;
const int _rightBracket = 0x5d;
const int _zero = 0x30;
const int _nine = 0x39;

/// Scans a bracketed index starting at the `[` at [at]. Returns the segment
/// and the index just past the closing `]`, after checking that whatever
/// follows the bracket is legal.
(DuetSegment, int) _scanIndex(String s, int at, bool expectKey) {
  if (expectKey) {
    throw DuetCodecException.badPath("unexpected '[' at offset $at");
  }
  final int start = at + 1;
  final int end = s.indexOf(']', start);
  if (end < 0) {
    throw DuetCodecException.badPath("unclosed '[' at offset $at");
  }
  final String raw = s.substring(start, end);
  final int? index = _canonicalIndex(raw);
  if (index == null) {
    throw DuetCodecException.badPath(
      'invalid index ${echoBounded(raw)} in brackets opened at offset $at: '
      'expected a canonical non-negative decimal integer',
    );
  }

  final int next = end + 1;
  // After a closing `]`, only `.`, `[` or end-of-input may follow.
  if (next < s.length &&
      s.codeUnitAt(next) != _dot &&
      s.codeUnitAt(next) != _leftBracket) {
    throw DuetCodecException.badPath(
      'unexpected character ${echoBounded(s[next])} at offset $next',
    );
  }
  return (DuetIndexSegment(index), next);
}

/// Parses the text between brackets, or null if it is not a canonical
/// non-negative decimal integer that fits in a Dart `int`.
int? _canonicalIndex(String raw) {
  if (raw.isEmpty) return null;
  for (final int c in raw.codeUnits) {
    if (c < _zero || c > _nine) return null;
  }
  if (raw.length > 1 && raw.startsWith('0')) return null;
  // `int.tryParse` returns null past 2^63-1, which is the Dart analogue of
  // Rust's `usize` parse failing: an index that large is refused rather than
  // silently wrapped.
  return int.tryParse(raw);
}

/// Scans a key starting at [at]: any run of characters other than `.`, `[` or
/// `]`. Returns the segment and the index just past the last character taken.
(DuetSegment, int) _scanKey(String s, int at) {
  int end = at;
  while (end < s.length) {
    final int c = s.codeUnitAt(end);
    if (c == _dot || c == _leftBracket || c == _rightBracket) break;
    end += 1;
  }
  if (end < s.length && s.codeUnitAt(end) == _rightBracket) {
    throw DuetCodecException.badPath("unexpected character ']' at offset $end");
  }
  if (end == at) {
    throw DuetCodecException.badPath(
      'expected a key at offset $at, found none',
    );
  }
  return (DuetKeySegment(s.substring(at, end)), end);
}
