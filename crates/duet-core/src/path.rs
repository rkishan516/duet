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
    pub fn from_segments(segments: Vec<Segment>) -> Self {
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
    /// The empty string parses to the root path.
    pub fn parse(s: &str) -> Result<Path, PathParseError> {
        if s.is_empty() {
            return Ok(Path::root());
        }

        let bytes = s.as_bytes();
        let mut segments = Vec::new();
        let mut i = 0usize;
        // True when the previous character was a `.`, meaning a key must follow.
        let mut expect_key = true;

        while i < bytes.len() {
            if bytes[i] == b'[' {
                let start = i + 1;
                let end = s[start..]
                    .find(']')
                    .map(|offset| start + offset)
                    .ok_or(PathParseError::UnclosedIndex(i))?;
                let raw = &s[start..end];
                let index = raw
                    .parse::<usize>()
                    .map_err(|_| PathParseError::InvalidIndex(raw.to_string()))?;
                segments.push(Segment::Index(index));
                i = end + 1;

                // After a closing `]`, only `.`, `[`, or end-of-input may follow.
                if i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    let ch = s[i..].chars().next().expect("i is a char boundary");
                    return Err(PathParseError::UnexpectedChar { at: i, ch });
                }
            } else {
                let mut end = i;
                while end < bytes.len()
                    && bytes[end] != b'.'
                    && bytes[end] != b'['
                    && bytes[end] != b']'
                {
                    end += 1;
                }
                if end < bytes.len() && bytes[end] == b']' {
                    return Err(PathParseError::UnexpectedChar { at: end, ch: ']' });
                }
                if end == i {
                    return Err(PathParseError::EmptySegment(i));
                }
                segments.push(Segment::Key(s[i..end].to_string()));
                i = end;
            }

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

/// Reasons a path string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathParseError {
    EmptySegment(usize),
    UnclosedIndex(usize),
    InvalidIndex(String),
    TrailingDot,
    /// A character appeared where the grammar does not allow it, such as a
    /// stray `]` or text immediately following a closing bracket.
    UnexpectedChar {
        at: usize,
        ch: char,
    },
}

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
            Err(PathParseError::InvalidIndex("bar".to_string()))
        );
    }

    #[test]
    fn display_round_trips_through_parse() {
        for raw in ["", "editor.zoom", "documents[3].title", "a[0][1].b"] {
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
        assert!(Path::parse("foo[3].bar").is_ok());
        assert!(Path::parse("foo[3][4]").is_ok());
        assert!(Path::parse("foo[3]").is_ok());
    }

    #[test]
    fn reports_multibyte_char_after_index_correctly() {
        // The error must carry the whole char, not a partial UTF-8 byte.
        assert_eq!(
            Path::parse("foo[3]é"),
            Err(PathParseError::UnexpectedChar { at: 6, ch: 'é' })
        );
    }
}
