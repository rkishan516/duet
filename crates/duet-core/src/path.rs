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
}

/// Reasons a path string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathParseError {
    EmptySegment(usize),
    UnclosedIndex(usize),
    InvalidIndex(String),
    TrailingDot,
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
}
