//! Paths addressing into the state tree.

/// One step in a path: either a map key or a list index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
    /// A string-keyed map lookup, e.g. the `editor` in `editor.zoom`.
    Key(String),
    /// A list index lookup, e.g. the `3` in `documents[3]`.
    Index(usize),
}

/// An address into the state tree. The empty path is the root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Path(Vec<Segment>);

impl Path {
    /// Returns the root path (the empty path).
    pub fn root() -> Self {
        Path(Vec::new())
    }

    /// Builds a path from an owned vector of segments, taking ownership of it.
    ///
    /// This performs no validation: [`Display`](std::fmt::Display) renders
    /// each `Segment::Key` verbatim, so the result round-trips through
    /// [`Path::parse`] only if every key is non-empty and avoids `.`, `[`,
    /// and `]`. Paths built by `parse` always satisfy this; hand-built paths
    /// must ensure it themselves. Debug builds assert it.
    ///
    /// This is for **trusted construction only** — the validation above is a
    /// `debug_assert!`, which is compiled out in release builds. A key
    /// containing `.` supplied by a guest process (Flutter, JavaScript) will
    /// panic a debug host, and in release will silently build a `Path` whose
    /// `Display` round-trips to a *different* path than intended (one
    /// segment in, two out on re-parse). Any path built from data arriving
    /// over IPC from a guest must go through [`Path::parse`] instead, which
    /// validates unconditionally in every build profile.
    pub fn from_segments(segments: Vec<Segment>) -> Self {
        debug_assert!(
            segments.iter().all(|segment| match segment {
                Segment::Key(k) => !k.is_empty() && !k.contains(['.', '[', ']']),
                Segment::Index(_) => true,
            }),
            "Segment::Key must be non-empty and must not contain '.', '[', or ']', or Display will not round-trip through parse"
        );
        Path(segments)
    }

    /// Borrows the path's segments as a slice.
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// Returns true if this path is the root (has no segments).
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Parses a path such as `editor.zoom` or `documents[3].title`.
    ///
    /// The grammar is a dot-separated sequence of keys, where a key may be
    /// followed by one or more bracketed indices (e.g. `documents[3][1]`).
    /// An index may not immediately follow a `.` — `a.[0]` is not legal
    /// syntax; write `a[0]` instead. A leading index with no preceding key
    /// (`[0]`) is legal. The empty string parses to the root path.
    ///
    /// A key is any run of characters other than `.`, `[`, or `]`,
    /// including whitespace; keys are not trimmed. For example `" "`,
    /// `"a b"`, `"\t"`, and `"🦀"` are all accepted as keys today.
    ///
    /// An index must be a canonical decimal integer: digits only, no
    /// leading `+` or `-`, and no leading zero unless the index is exactly
    /// `0`. `[007]`, `[+3]`, and `[]` are all rejected so that `parse` and
    /// [`Display`](std::fmt::Display) are mutually inverse — there is
    /// exactly one string representation for any given index.
    ///
    /// # Errors
    ///
    /// Returns [`PathParseError`] when `s` does not match the grammar above:
    /// - [`PathParseError::EmptySegment`] — a `.` was at the start of the
    ///   string, or one `.` immediately followed another `.`.
    /// - [`PathParseError::TrailingDot`] — the string ended immediately
    ///   after a `.`.
    /// - [`PathParseError::UnclosedIndex`] — a `[` had no matching `]`.
    /// - [`PathParseError::InvalidIndex`] — the text inside `[...]` was not
    ///   a canonical decimal integer (see above).
    /// - [`PathParseError::UnexpectedChar`] — a stray `]`, an index
    ///   immediately following a `.`, or text immediately after a `]` that
    ///   is not `.` or `[`.
    ///
    /// All offsets carried by [`PathParseError`] are **byte offsets** into
    /// `s`, not `char` offsets. Because this parser runs on paths arriving
    /// from Flutter and JavaScript guest processes over IPC, callers
    /// translating an offset back into a UTF-16 (JavaScript) or Dart string
    /// index must convert accordingly — a byte offset does not line up
    /// directly with either.
    pub fn parse(s: &str) -> Result<Path, PathParseError> {
        if s.is_empty() {
            return Ok(Path::root());
        }

        let bytes = s.as_bytes();
        let mut segments = Vec::new();
        let mut i = 0usize;
        // True immediately after consuming a `.`: the grammar requires a
        // key (not an index) to follow, since `a.[0]` is not legal syntax.
        let mut expect_key = false;

        while i < bytes.len() {
            let (segment, next) = if bytes[i] == b'[' {
                if expect_key {
                    return Err(PathParseError::UnexpectedChar { at: i, ch: '[' });
                }
                scan_index(s, i)?
            } else {
                scan_key(s, i)?
            };
            segments.push(segment);
            i = next;
            expect_key = false;

            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                expect_key = true;
            }
        }

        if expect_key {
            return Err(PathParseError::TrailingDot);
        }

        Ok(Path(segments))
    }

    /// True when `self` addresses this path or any ancestor of `other`.
    #[inline]
    pub fn is_prefix_of(&self, other: &Path) -> bool {
        self.0.len() <= other.0.len() && self.0.iter().zip(other.0.iter()).all(|(a, b)| a == b)
    }

    /// True when either path is a prefix of the other.
    ///
    /// This is the subscription-matching rule: a subscriber at `self` must be
    /// notified about a write at `other` when the paths overlap in either
    /// direction. A write to an ancestor may change the subscriber's value, and a
    /// write to a descendant changes part of the subtree the subscriber observes.
    #[inline]
    pub fn overlaps(&self, other: &Path) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }
}

/// Parses an index segment starting at the `[` found at byte offset `at`.
/// Returns the segment and the byte offset just past the closing `]`, after
/// checking that whatever follows the bracket is legal.
fn scan_index(s: &str, at: usize) -> Result<(Segment, usize), PathParseError> {
    debug_assert!(s.is_char_boundary(at));
    debug_assert_eq!(s.as_bytes().get(at), Some(&b'['));

    let bytes = s.as_bytes();
    let start = at + 1;
    let end = s[start..]
        .find(']')
        .map(|offset| start + offset)
        .ok_or(PathParseError::UnclosedIndex(at))?;
    let raw = &s[start..end];

    let canonical = !raw.is_empty()
        && raw.bytes().all(|b| b.is_ascii_digit())
        && (raw.len() == 1 || !raw.starts_with('0'));
    if !canonical {
        return Err(PathParseError::InvalidIndex {
            at,
            raw: raw.to_string(),
        });
    }
    let index: usize = raw.parse().map_err(|_| PathParseError::InvalidIndex {
        at,
        raw: raw.to_string(),
    })?;

    let next = end + 1;
    // After a closing `]`, only `.`, `[`, or end-of-input may follow.
    if next < bytes.len() && bytes[next] != b'.' && bytes[next] != b'[' {
        let ch = s[next..].chars().next().expect("next is a char boundary");
        return Err(PathParseError::UnexpectedChar { at: next, ch });
    }

    Ok((Segment::Index(index), next))
}

/// Parses a key segment starting at byte offset `at`. A key is any run of
/// characters other than `.`, `[`, or `]`. Returns the segment and the byte
/// offset just past the last character consumed.
fn scan_key(s: &str, at: usize) -> Result<(Segment, usize), PathParseError> {
    debug_assert!(s.is_char_boundary(at));
    debug_assert_ne!(s.as_bytes().get(at), Some(&b'['));

    let bytes = s.as_bytes();
    let mut end = at;
    while end < bytes.len() && bytes[end] != b'.' && bytes[end] != b'[' && bytes[end] != b']' {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b']' {
        return Err(PathParseError::UnexpectedChar { at: end, ch: ']' });
    }
    if end == at {
        return Err(PathParseError::EmptySegment(at));
    }
    Ok((Segment::Key(s[at..end].to_string()), end))
}

/// Reasons a path string could not be parsed by [`Path::parse`].
///
/// All offsets carried by these variants are **byte offsets** into the
/// original input, not `char` offsets — see the byte-offset note on
/// [`Path::parse`] for why that matters when relaying an error back to a
/// non-Rust guest process.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathParseError {
    /// A key was expected at this byte offset, but a `.` was found there
    /// instead — either at the very start of the input, or immediately
    /// after another `.`.
    EmptySegment(usize),
    /// The `[` at this byte offset was never matched by a closing `]`.
    UnclosedIndex(usize),
    /// A bracket contained something that is not a canonical decimal index.
    InvalidIndex {
        /// The byte offset of the opening `[`.
        at: usize,
        /// The raw text found between the brackets, unparsed.
        raw: String,
    },
    /// The path ended immediately after a `.`, with no key following it.
    TrailingDot,
    /// A character appeared where the grammar does not allow it: a stray
    /// `]`, a `[` immediately following a `.`, or text immediately after a
    /// closing `]` that is not `.` or `[`.
    UnexpectedChar {
        /// The byte offset of the unexpected character.
        at: usize,
        /// The unexpected character itself.
        ch: char,
    },
}

impl std::fmt::Display for PathParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathParseError::EmptySegment(at) => {
                write!(f, "expected a key at byte offset {at}, found none")
            }
            PathParseError::UnclosedIndex(at) => {
                write!(f, "unclosed '[' at byte offset {at}")
            }
            PathParseError::InvalidIndex { at, raw } => {
                // `raw` echoes guest-supplied input and is unbounded; cap
                // what we render so a pathological input (e.g. a[<1 MB of
                // digits>]) can't produce a huge message that gets relayed
                // back to the guest or written to host logs. The full value
                // is still available via `Debug` or the struct field.
                let char_count = raw.chars().count();
                let mut display_raw: String = raw.chars().take(32).collect();
                if char_count > 32 {
                    display_raw.push('…');
                }
                write!(
                    f,
                    "invalid index {display_raw:?} in brackets opened at byte offset {at}: \
                     expected a canonical non-negative decimal integer"
                )
            }
            PathParseError::TrailingDot => {
                write!(f, "path ends with a trailing '.' with no key following it")
            }
            PathParseError::UnexpectedChar { at, ch } => {
                write!(f, "unexpected character {ch:?} at byte offset {at}")
            }
        }
    }
}

impl std::error::Error for PathParseError {}

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for segment in &self.0 {
            match segment {
                Segment::Key(k) => {
                    if !first {
                        write!(f, ".")?;
                    }
                    write!(f, "{k}")?;
                }
                Segment::Index(i) => write!(f, "[{i}]")?,
            }
            first = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn root_path_is_empty() {
        let p = Path::root();
        assert!(p.is_root());
        assert_eq!(p.segments().len(), 0);
    }

    #[test]
    fn path_from_segments() {
        let segments = vec![
            Segment::Key("editor".to_string()),
            Segment::Key("zoom".to_string()),
        ];
        let p = Path::from_segments(segments.clone());
        assert!(!p.is_root());
        assert_eq!(p.segments(), segments.as_slice());
    }

    #[test]
    #[should_panic(expected = "must not contain")]
    fn from_segments_rejects_dotted_key() {
        Path::from_segments(vec![Segment::Key("a.b".to_string())]);
    }

    #[test]
    #[should_panic(expected = "must not contain")]
    fn from_segments_rejects_empty_key() {
        Path::from_segments(vec![Segment::Key(String::new())]);
    }

    #[test]
    fn parses_empty_string_as_root() {
        assert_eq!(Path::parse("").unwrap(), Path::root());
    }

    #[test]
    fn parses_dotted_keys() {
        let p = Path::parse("editor.zoom").unwrap();
        assert_eq!(
            p.segments(),
            &[
                Segment::Key("editor".to_string()),
                Segment::Key("zoom".to_string())
            ]
        );
    }

    #[test]
    fn parses_index_and_key_mix() {
        let p = Path::parse("documents[3].title").unwrap();
        assert_eq!(
            p.segments(),
            &[
                Segment::Key("documents".to_string()),
                Segment::Index(3),
                Segment::Key("title".to_string())
            ]
        );
    }

    #[test]
    fn rejects_leading_dot() {
        assert_eq!(Path::parse(".foo"), Err(PathParseError::EmptySegment(0)));
    }

    #[test]
    fn rejects_trailing_dot() {
        assert_eq!(Path::parse("foo."), Err(PathParseError::TrailingDot));
    }

    #[test]
    fn rejects_unclosed_index() {
        assert_eq!(Path::parse("foo[3"), Err(PathParseError::UnclosedIndex(3)));
    }

    #[test]
    fn rejects_non_numeric_index() {
        assert_eq!(
            Path::parse("foo[bar]"),
            Err(PathParseError::InvalidIndex {
                at: 3,
                raw: "bar".to_string()
            })
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        for raw in ["", "[0]", "editor.zoom", "documents[3].title", "a[0][1].b"] {
            let parsed = Path::parse(raw).unwrap();
            assert_eq!(parsed.to_string(), raw, "round trip failed for {raw:?}");
        }
    }

    #[test]
    fn rejects_stray_closing_bracket() {
        assert_eq!(
            Path::parse("foo]"),
            Err(PathParseError::UnexpectedChar { at: 3, ch: ']' })
        );
    }

    #[test]
    fn rejects_text_immediately_after_index() {
        assert_eq!(
            Path::parse("foo[3]extra"),
            Err(PathParseError::UnexpectedChar { at: 6, ch: 'e' })
        );
    }

    #[test]
    fn allows_legal_characters_after_index() {
        // `.`, `[`, and end-of-input are all legal after a closing bracket.
        assert_eq!(Path::parse("foo[3].bar").unwrap().to_string(), "foo[3].bar");
        assert_eq!(Path::parse("foo[3][4]").unwrap().to_string(), "foo[3][4]");
        assert_eq!(Path::parse("foo[3]").unwrap().to_string(), "foo[3]");
    }

    #[test]
    fn reports_multibyte_char_after_index_correctly() {
        // The error must carry the whole char, not a partial UTF-8 byte.
        assert_eq!(
            Path::parse("foo[3]é"),
            Err(PathParseError::UnexpectedChar { at: 6, ch: 'é' })
        );
    }

    #[test]
    fn allows_leading_index() {
        let p = Path::parse("[0]").unwrap();
        assert_eq!(p.segments(), &[Segment::Index(0)]);
        assert_eq!(p.to_string(), "[0]");
    }

    #[test]
    fn rejects_index_immediately_after_dot() {
        assert_eq!(
            Path::parse("a.[0]"),
            Err(PathParseError::UnexpectedChar { at: 2, ch: '[' })
        );
    }

    #[test]
    fn rejects_leading_zero_index() {
        assert_eq!(
            Path::parse("a[007]"),
            Err(PathParseError::InvalidIndex {
                at: 1,
                raw: "007".to_string()
            })
        );
    }

    #[test]
    fn rejects_plus_prefixed_index() {
        assert_eq!(
            Path::parse("a[+3]"),
            Err(PathParseError::InvalidIndex {
                at: 1,
                raw: "+3".to_string()
            })
        );
    }

    #[test]
    fn rejects_empty_index() {
        assert_eq!(
            Path::parse("foo[]"),
            Err(PathParseError::InvalidIndex {
                at: 3,
                raw: String::new()
            })
        );
    }

    #[test]
    fn rejects_negative_index() {
        assert_eq!(
            Path::parse("foo[-1]"),
            Err(PathParseError::InvalidIndex {
                at: 3,
                raw: "-1".to_string()
            })
        );
    }

    #[test]
    fn rejects_double_dot() {
        assert_eq!(Path::parse("a..b"), Err(PathParseError::EmptySegment(2)));
    }

    #[test]
    fn rejects_index_larger_than_usize() {
        // "18446744073709551616" is all digits with no leading zero, so it
        // passes the canonical-digits check; the only way it can still fail
        // is the `parse::<usize>()` overflowing (it is 2^64, one past
        // u64::MAX / usize::MAX on a 64-bit target). This pins that the
        // overflow arm is genuinely reachable.
        assert_eq!(
            Path::parse("a[18446744073709551616]"),
            Err(PathParseError::InvalidIndex {
                at: 1,
                raw: "18446744073709551616".to_string()
            })
        );
        // The largest value that does fit must still be accepted.
        assert!(Path::parse("a[18446744073709551615]").is_ok());
    }

    #[test]
    fn path_parse_error_messages_are_informative() {
        assert_eq!(
            PathParseError::EmptySegment(4).to_string(),
            "expected a key at byte offset 4, found none"
        );
        assert_eq!(
            PathParseError::UnclosedIndex(7).to_string(),
            "unclosed '[' at byte offset 7"
        );
        assert_eq!(
            PathParseError::InvalidIndex {
                at: 2,
                raw: "bar".to_string()
            }
            .to_string(),
            "invalid index \"bar\" in brackets opened at byte offset 2: \
             expected a canonical non-negative decimal integer"
        );
        assert_eq!(
            PathParseError::TrailingDot.to_string(),
            "path ends with a trailing '.' with no key following it"
        );
        assert_eq!(
            PathParseError::UnexpectedChar { at: 5, ch: ']' }.to_string(),
            "unexpected character ']' at byte offset 5"
        );
    }

    #[test]
    fn invalid_index_display_is_bounded_but_struct_field_is_not() {
        let long_raw = "x".repeat(100);
        let input = format!("a[{long_raw}]");
        let err = Path::parse(&input).unwrap_err();

        let PathParseError::InvalidIndex { at, raw } = &err else {
            panic!("expected InvalidIndex, got {err:?}");
        };
        assert_eq!(*at, 1);
        // The struct field must retain the full, untruncated value.
        assert_eq!(raw.len(), 100);

        // The rendered message must be bounded to 32 chars of `raw` plus an
        // ellipsis marker, not all 100. Isolate the quoted `raw` echo
        // specifically, rather than counting 'x' over the whole message —
        // words like "index" and "expected" contain 'x' too.
        let rendered = err.to_string();
        let quote_start = rendered.find('"').expect("message quotes the raw value") + 1;
        let quote_end = quote_start
            + rendered[quote_start..]
                .find('"')
                .expect("closing quote present");
        let quoted = &rendered[quote_start..quote_end];
        assert_eq!(
            quoted.chars().filter(|&c| c == 'x').count(),
            32,
            "quoted raw value should carry exactly 32 of the original 100 'x's: {quoted:?}"
        );
        assert!(
            quoted.ends_with('…'),
            "truncated output should carry an ellipsis marker: {quoted:?}"
        );
        // Exactly 32 'x's plus one ellipsis char, regardless of `raw`'s
        // actual length (100 here, but the bound holds for any length).
        assert_eq!(quoted.chars().count(), 33);
    }

    #[test]
    fn parses_multibyte_key() {
        let p = Path::parse("café.zoom").unwrap();
        assert_eq!(
            p.segments(),
            &[
                Segment::Key("café".to_string()),
                Segment::Key("zoom".to_string())
            ]
        );
        assert_eq!(p.to_string(), "café.zoom");
    }

    /// Exhaustively verifies that every input `parse` accepts renders back to
    /// exactly that input. This is the invariant the strict grammar exists to
    /// guarantee; a hole here means a client path can silently become a
    /// different path.
    ///
    /// Enumerates every string of length 0..=5 over a 7-char alphabet
    /// (`'1'` is included so a valid multi-digit index like `[10]` is
    /// actually reachable and exercised, not just single-digit and
    /// rejected multi-`0` indices).
    #[test]
    fn round_trip_is_total_over_short_inputs() {
        const ALPHABET: [char; 7] = ['a', '.', '[', ']', '0', '1', 'é'];
        let mut accepted = 0usize;

        for len in 0..=5 {
            for mut code in 0..ALPHABET.len().pow(len as u32) {
                let candidate: String = (0..len)
                    .map(|_| {
                        let c = ALPHABET[code % ALPHABET.len()];
                        code /= ALPHABET.len();
                        c
                    })
                    .collect();
                if let Ok(path) = Path::parse(&candidate) {
                    accepted += 1;
                    assert_eq!(
                        path.to_string(),
                        candidate,
                        "round trip failed for {candidate:?} -> {:?}",
                        path.segments()
                    );
                }
            }
        }

        // Pinned to the exact count so a regression that silently narrows
        // the accepted language (e.g. rejecting two-thirds of valid inputs)
        // cannot pass by accident. If this number changes, the accepted
        // language changed — update it deliberately, with an explanation.
        assert_eq!(
            accepted, 2405,
            "accepted count changed; the language `parse` accepts changed \
             (see the comment above this assertion)"
        );
    }

    #[test]
    fn prefix_is_directional() {
        assert!(p("editor").is_prefix_of(&p("editor.zoom")));
        assert!(!p("editor.zoom").is_prefix_of(&p("editor")));
    }

    #[test]
    fn path_is_its_own_prefix() {
        assert!(p("editor.zoom").is_prefix_of(&p("editor.zoom")));
    }

    #[test]
    fn root_is_prefix_of_everything() {
        assert!(Path::root().is_prefix_of(&p("a.b[2].c")));
        assert!(Path::root().is_prefix_of(&Path::root()));
    }

    #[test]
    fn overlaps_is_bidirectional() {
        // Ancestor write reaches descendant subscriber.
        assert!(p("editor").overlaps(&p("editor.zoom")));
        // Descendant write reaches ancestor subscriber.
        assert!(p("editor.zoom").overlaps(&p("editor")));
    }

    #[test]
    fn siblings_do_not_overlap() {
        assert!(!p("editor.zoom").overlaps(&p("editor.theme")));
    }

    #[test]
    fn distinct_indices_do_not_overlap() {
        assert!(!p("docs[0].title").overlaps(&p("docs[1].title")));
    }

    #[test]
    fn key_that_is_string_prefix_of_another_does_not_overlap() {
        // A subscriber at `edit` must not be notified about a write to `editor`.
        // Segment equality is exact, not textual-prefix.
        assert!(!p("edit").overlaps(&p("editor")));
        assert!(!p("editor").overlaps(&p("edit")));
        assert!(!p("edit").is_prefix_of(&p("editor")));
        assert!(!p("editor").is_prefix_of(&p("edit")));
    }

    /// Enumerates every path of depth `0..=MAX_DEPTH` over a 4-symbol
    /// alphabet (`a`, `ab`, `[0]`, `[1]`) at every segment position. This
    /// guarantees coverage of: the root path (depth 0); single-key and
    /// single-index paths (depth 1: `a`, `ab`, `[0]`, `[1]`); depth-2 and
    /// depth-3 paths; sibling keys at the same depth (e.g. `a` vs `ab`);
    /// distinct indices at the same position (e.g. `[0]` vs `[1]`); a key
    /// and an index at the same depth position (e.g. `a` vs `[0]`); and,
    /// critically, a key that is a *string*-prefix of another key at the
    /// same position (`a` is a string-prefix of `ab`) — this is what
    /// distinguishes correct `Segment` equality from an accidental
    /// `str::starts_with` bug, which a corpus of single-character keys
    /// cannot catch, since every key would then be equal-length and
    /// distinguishing the two implementations would be impossible.
    fn prefix_test_corpus() -> Vec<Path> {
        const MAX_DEPTH: usize = 3;

        // `"a"` and `"ab"` deliberately share a string prefix — do not
        // collapse `"ab"` back to a single character like `"b"`. A future
        // edit that does so would silently reopen the string-prefix-vs-
        // equality gap described in the doc comment above, since a corpus
        // of single-character keys can't distinguish `==` from
        // `starts_with`.
        let alphabet: [Segment; 4] = [
            Segment::Key("a".to_string()),
            Segment::Key("ab".to_string()),
            Segment::Index(0),
            Segment::Index(1),
        ];

        let mut corpus = Vec::new();
        for len in 0..=MAX_DEPTH {
            let total = alphabet.len().pow(len as u32);
            for code in 0..total {
                let mut remaining = code;
                let mut segments = Vec::with_capacity(len);
                for _ in 0..len {
                    segments.push(alphabet[remaining % alphabet.len()].clone());
                    remaining /= alphabet.len();
                }
                corpus.push(Path::from_segments(segments));
            }
        }

        // One path per segment sequence of every length 0..=MAX_DEPTH over
        // the alphabet: sum_{d=0}^{MAX_DEPTH} alphabet.len()^d. Derived
        // rather than hardcoded so the asserted count can never drift from
        // what the loop above actually produces.
        let expected: usize = (0..=MAX_DEPTH).map(|d| alphabet.len().pow(d as u32)).sum();
        assert_eq!(
            corpus.len(),
            expected,
            "corpus size changed; update this count deliberately"
        );
        corpus
    }

    /// `overlaps` must be symmetric: if A overlaps B then B overlaps A.
    /// Subscription routing is meaningless if this does not hold.
    #[test]
    fn overlaps_is_symmetric_over_generated_paths() {
        let corpus = prefix_test_corpus();
        for a in &corpus {
            for b in &corpus {
                assert_eq!(
                    a.overlaps(b),
                    b.overlaps(a),
                    "symmetry broken for {a} vs {b}"
                );
            }
        }
    }

    /// Every path overlaps itself, and root overlaps everything.
    #[test]
    fn overlaps_is_reflexive_and_root_is_universal() {
        for a in &prefix_test_corpus() {
            assert!(a.overlaps(a), "{a} does not overlap itself");
            assert!(Path::root().overlaps(a), "root does not overlap {a}");
            assert!(a.overlaps(&Path::root()), "{a} does not overlap root");
        }
    }

    /// `is_prefix_of` must be transitive: a prefix of a prefix is a prefix.
    #[test]
    fn is_prefix_of_is_transitive() {
        let corpus = prefix_test_corpus();
        for a in &corpus {
            for b in &corpus {
                for c in &corpus {
                    if a.is_prefix_of(b) && b.is_prefix_of(c) {
                        assert!(a.is_prefix_of(c), "transitivity broken: {a} -> {b} -> {c}");
                    }
                }
            }
        }
    }

    /// `is_prefix_of` must be antisymmetric: mutual prefixes are equal.
    #[test]
    fn is_prefix_of_is_antisymmetric() {
        let corpus = prefix_test_corpus();
        for a in &corpus {
            for b in &corpus {
                if a.is_prefix_of(b) && b.is_prefix_of(a) {
                    assert_eq!(a, b, "mutual prefixes must be equal: {a} vs {b}");
                }
            }
        }
    }
}
