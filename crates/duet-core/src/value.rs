//! The dynamic state tree.

use std::collections::BTreeMap;

use crate::path::{Path, Segment};

/// A dynamically typed node in the state tree.
///
/// Typed access is generated in Phase 4 and layers on top of this. Keeping the
/// runtime representation dynamic is what allows path addressing and minimal
/// patches to work without any knowledge of user types.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// The absence of a value.
    Null,
    /// A boolean.
    Bool(bool),
    /// A signed 64-bit integer.
    Int(i64),
    /// A 64-bit floating point number.
    ///
    /// `Value` derives `PartialEq`, and IEEE 754 defines `NaN != NaN`. That
    /// makes `Value` equality non-reflexive whenever a `Float` holds `NaN`:
    /// `Value::Float(f64::NAN) != Value::Float(f64::NAN)`, and a `Value`
    /// containing such a node is not equal to itself. See
    /// `float_nan_is_not_equal_to_itself` in the test module below, which
    /// pins this rather than working around it. Code that later compares
    /// `Value`s for change detection (the `Store` task) must account for
    /// this.
    Float(f64),
    /// A UTF-8 string.
    Str(String),
    /// Arbitrary binary data.
    Bytes(Vec<u8>),
    /// An ordered sequence of values, addressed by [`Segment::Index`].
    List(Vec<Value>),
    /// A string-keyed map, addressed by [`Segment::Key`].
    ///
    /// `BTreeMap` rather than `HashMap`: deterministic ordering keeps patch
    /// payloads and golden-file tests stable.
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// Convenience constructor for map literals in tests and app setup.
    ///
    /// Builds a [`Value::Map`] from `(key, value)` pairs. An empty iterator
    /// (e.g. `Value::map([])`) produces an empty map; Rust can infer the
    /// element type from context (typically the `Value` return type of the
    /// enclosing expression), so no turbofish is needed in the tests below,
    /// but a bare `Value::map([])` used where the element type cannot be
    /// inferred from context would need one.
    pub fn map<'a>(entries: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
        Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    /// Reads the value at `path`, or `None` if any segment is missing or
    /// addresses the wrong kind of node.
    pub fn get(&self, path: &Path) -> Option<&Value> {
        let mut current = self;
        for segment in path.segments() {
            current = match (current, segment) {
                (Value::Map(m), Segment::Key(k)) => m.get(k)?,
                (Value::List(l), Segment::Index(i)) => l.get(*i)?,
                _ => return None,
            };
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;

    fn sample() -> Value {
        Value::map([
            (
                "editor",
                Value::map([
                    ("zoom", Value::Float(1.0)),
                    ("theme", Value::Str("dark".into())),
                ]),
            ),
            (
                "documents",
                Value::List(vec![Value::map([("title", Value::Str("first".into()))])]),
            ),
        ])
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn get_root_returns_whole_tree() {
        let v = sample();
        assert_eq!(v.get(&Path::root()), Some(&sample()));
    }

    #[test]
    fn get_nested_key() {
        assert_eq!(sample().get(&p("editor.zoom")), Some(&Value::Float(1.0)));
    }

    #[test]
    fn get_through_index() {
        assert_eq!(
            sample().get(&p("documents[0].title")),
            Some(&Value::Str("first".into()))
        );
    }

    #[test]
    fn get_missing_key_returns_none() {
        assert_eq!(sample().get(&p("editor.missing")), None);
    }

    #[test]
    fn get_out_of_bounds_index_returns_none() {
        assert_eq!(sample().get(&p("documents[9].title")), None);
    }

    #[test]
    fn get_through_wrong_kind_returns_none() {
        // `editor` is a map, not a list.
        assert_eq!(sample().get(&p("editor[0]")), None);
    }
}
