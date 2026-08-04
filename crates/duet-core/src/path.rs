//! Paths addressing into the state tree.

/// One step in a path: either a map key or a list index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
    Key(String),
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
    /// [`Path::parse`] only if every key avoids `.`, `[`, and `]`. Paths
    /// built by `parse` always satisfy this; hand-built paths must ensure it
    /// themselves. Debug builds assert it.
    pub fn from_segments(segments: Vec<Segment>) -> Self {
        debug_assert!(
            segments.iter().all(|segment| match segment {
                Segment::Key(k) => !k.contains(['.', '[', ']']),
                Segment::Index(_) => true,
            }),
            "Segment::Key must not contain '.', '[', or ']', or Display will not round-trip through parse"
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
    ///   string, or one `.` immediately followed another `.` or a `[`.
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
}

/// Parses an index segment starting at the `[` found at byte offset `at`.
/// Returns the segment and the byte offset just past the closing `]`, after
/// checking that whatever follows the bracket is legal.
fn scan_index(s: &str, at: usize) -> Result<(Segment, usize), PathParseError> {
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
    /// A key was expected starting at this byte offset, but the grammar
    /// found a `.` (or the end of input) there instead — e.g. a leading
    /// `.`, or two `.` in a row.
    EmptySegment(usize),
    /// The `[` at this byte offset was never matched by a closing `]`.
    UnclosedIndex(usize),
    /// A bracket contained something that is not a canonical decimal index.
    /// `at` is the byte offset of the opening `[`.
    InvalidIndex { at: usize, raw: String },
    /// The path ended immediately after a `.`, with no key following it.
    TrailingDot,
    /// A character appeared where the grammar does not allow it, such as a
    /// stray `]` or text immediately following a closing bracket.
    UnexpectedChar { at: usize, ch: char },
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
                write!(
                    f,
                    "invalid index {raw:?} in brackets opened at byte offset {at}: \
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
    #[test]
    fn round_trip_is_total_over_short_inputs() {
        const ALPHABET: [char; 6] = ['a', '.', '[', ']', '0', 'é'];
        let mut accepted = 0usize;

        for len in 0..=4 {
            let mut indices = vec![0usize; len];
            loop {
                let candidate: String = indices.iter().map(|&i| ALPHABET[i]).collect();
                if let Ok(path) = Path::parse(&candidate) {
                    accepted += 1;
                    assert_eq!(
                        path.to_string(),
                        candidate,
                        "round trip failed for {candidate:?} -> {:?}",
                        path.segments()
                    );
                }

                if len == 0 {
                    break;
                }
                let mut pos = len;
                loop {
                    if pos == 0 {
                        break;
                    }
                    pos -= 1;
                    indices[pos] += 1;
                    if indices[pos] < ALPHABET.len() {
                        break;
                    }
                    indices[pos] = 0;
                    if pos == 0 {
                        break;
                    }
                }
                if indices.iter().all(|&i| i == 0) {
                    break;
                }
            }
        }

        assert!(
            accepted > 50,
            "expected a meaningful number of accepted inputs, got {accepted}"
        );
    }
}
