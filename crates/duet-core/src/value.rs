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
    ///
    /// If the same key appears more than once, the last occurrence wins —
    /// this is plain `BTreeMap` insertion semantics, not special-cased here.
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
    ///
    /// `get` collapses every failure reason into `None`, unlike [`Value::set`]
    /// which returns a [`SetError`] describing exactly what went wrong. This
    /// asymmetry is deliberate: `get` is the read hot path, and `Option`
    /// keeps it allocation-free with no error type to construct, while `set`
    /// is comparatively rare and its callers benefit from knowing why a
    /// write was rejected.
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

    /// Writes `value` at `path`.
    ///
    /// Intermediate nodes are never created: writing to `a.b` when `a` does not
    /// exist is a `MissingKey` error rather than an implicit insert. The final
    /// segment of a map path *is* inserted if absent, so adding a new key to an
    /// existing map succeeds. The empty path (root) replaces the whole tree
    /// unconditionally and cannot fail.
    ///
    /// On error, `self` is left completely unchanged: `set` only mutates the
    /// tree after it has walked every intermediate segment successfully, so a
    /// failure partway through never leaves a partial write in place. This
    /// matters to the `Store` task: a failed write must produce no
    /// notifications and no partial mutation.
    ///
    /// # Errors
    ///
    /// - [`SetError::MissingKey`] — an intermediate or final map key on the
    ///   path does not exist. Note the final segment of a map path is the one
    ///   exception: it is inserted rather than erroring (see above), so this
    ///   variant is only ever returned for a key strictly before the last
    ///   segment.
    /// - [`SetError::IndexOutOfBounds`] — a list index on the path, whether
    ///   intermediate or final, is `>= len()` for the list it addresses. This
    ///   includes an index exactly equal to `len()`; `set` never appends.
    /// - [`SetError::TypeMismatch`] — a segment addressed the wrong kind of
    ///   node, e.g. a key segment against a `List`, an index segment against
    ///   a `Map`, or any segment against a scalar (`Null`, `Bool`, `Int`,
    ///   `Float`, `Str`, `Bytes`).
    ///
    /// [`SetError::MissingKey`] and [`SetError::TypeMismatch`] carry the
    /// *full* path passed to `set`, not the partial path walked so far — a
    /// guest process relaying the error over IPC needs the whole address to
    /// locate the problem, not just the segment where the walk stopped.
    /// [`SetError::IndexOutOfBounds`] is the exception: it carries only the
    /// offending index, not a path (see that variant's doc comment).
    pub fn set(&mut self, path: &Path, value: Value) -> Result<(), SetError> {
        let segments = path.segments();
        let Some((last, parents)) = segments.split_last() else {
            *self = value;
            return Ok(());
        };

        let mut current: &mut Value = self;
        for segment in parents {
            current = match (current, segment) {
                (Value::Map(m), Segment::Key(k)) => m
                    .get_mut(k)
                    .ok_or_else(|| SetError::MissingKey(path.clone()))?,
                (Value::List(l), Segment::Index(i)) => {
                    l.get_mut(*i).ok_or(SetError::IndexOutOfBounds(*i))?
                }
                _ => return Err(SetError::TypeMismatch(path.clone())),
            };
        }

        match (current, last) {
            (Value::Map(m), Segment::Key(k)) => {
                // Clone `k` only on a genuine insert of a new key; an
                // overwrite of an existing key reuses its `String` in place.
                // This is the hot path for `Store::set`, so avoiding an
                // allocation on every overwrite matters.
                match m.get_mut(k) {
                    Some(slot) => *slot = value,
                    None => {
                        m.insert(k.clone(), value);
                    }
                }
                Ok(())
            }
            (Value::List(l), Segment::Index(i)) => {
                let slot = l.get_mut(*i).ok_or(SetError::IndexOutOfBounds(*i))?;
                *slot = value;
                Ok(())
            }
            _ => Err(SetError::TypeMismatch(path.clone())),
        }
    }
}

/// Why a write ([`Value::set`]) could not be applied.
///
/// [`SetError::MissingKey`] and [`SetError::TypeMismatch`] carry the full
/// path originally passed to `set`, not a partial path, so a guest process
/// relaying the error over IPC can locate the problem without reconstructing
/// context this side of the boundary. [`SetError::IndexOutOfBounds`] does
/// **not** carry a path — see its doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    /// A map key on the path does not exist. Only possible for a key
    /// strictly before the final segment: the final segment of a map path is
    /// inserted rather than erroring, so a missing *final* key never
    /// produces this variant.
    MissingKey(Path),
    /// A list index on the path — intermediate or final — is out of bounds
    /// for the list it addresses. An index equal to `len()` counts as out of
    /// bounds; `set` never appends.
    ///
    /// Unlike the other two variants, this one carries only the offending
    /// index, **not** the path that produced it: two different paths that
    /// bottom out on the same out-of-bounds index (e.g. `a.b.c[9]` and
    /// `flags[9]`) produce an identical, indistinguishable
    /// `IndexOutOfBounds(9)`. A caller that needs the full address for a
    /// guest-facing error message must pair this with the path it originally
    /// passed to `set`.
    IndexOutOfBounds(usize),
    /// A segment addressed the wrong kind of node: a key segment against a
    /// `List`, an index segment against a `Map`, or any segment against a
    /// scalar variant.
    TypeMismatch(Path),
}

impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetError::MissingKey(path) => write!(f, "no key exists at path \"{path}\""),
            SetError::IndexOutOfBounds(i) => write!(f, "index {i} is out of bounds"),
            SetError::TypeMismatch(path) => {
                write!(f, "path \"{path}\" addresses the wrong kind of node")
            }
        }
    }
}

impl std::error::Error for SetError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;

    /// The fixture used across `value` tests.
    ///
    /// Deliberately reaches structural cases a smaller fixture cannot:
    /// - `documents` has 3 elements (not 1), so a `set`/`get` that always
    ///   touches slot 0 regardless of the requested index is distinguishable
    ///   from a correct implementation, and an out-of-bounds *intermediate*
    ///   index (e.g. `documents[9].title`) is reachable.
    /// - `matrix` is a list nested in a list.
    /// - `empty_map` and `empty_list` are empty containers.
    /// - `thumbnail` is a `Bytes` value.
    ///
    /// `editor.zoom`, `editor.theme`, and `documents[0].title` keep their
    /// original values so existing tests that reference them by path are
    /// unaffected by this fixture's growth.
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
                Value::List(vec![
                    Value::map([("title", Value::Str("first".into()))]),
                    Value::map([("title", Value::Str("second".into()))]),
                    Value::map([("title", Value::Str("third".into()))]),
                ]),
            ),
            (
                "matrix",
                Value::List(vec![
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                    Value::List(vec![Value::Int(3)]),
                ]),
            ),
            ("empty_map", Value::map([])),
            ("empty_list", Value::List(vec![])),
            ("thumbnail", Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF])),
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

    #[test]
    fn set_root_replaces_whole_tree() {
        let mut v = sample();
        v.set(&Path::root(), Value::Int(7)).unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn set_existing_key() {
        let mut v = sample();
        v.set(&p("editor.zoom"), Value::Float(2.5)).unwrap();
        assert_eq!(v.get(&p("editor.zoom")), Some(&Value::Float(2.5)));
    }

    #[test]
    fn set_inserts_new_key_on_existing_map() {
        let mut v = sample();
        v.set(&p("editor.wrap"), Value::Bool(true)).unwrap();
        assert_eq!(v.get(&p("editor.wrap")), Some(&Value::Bool(true)));
    }

    #[test]
    fn set_through_index() {
        let mut v = sample();
        v.set(&p("documents[0].title"), Value::Str("renamed".into()))
            .unwrap();
        assert_eq!(
            v.get(&p("documents[0].title")),
            Some(&Value::Str("renamed".into()))
        );
    }

    #[test]
    fn set_missing_intermediate_key_errors() {
        let mut v = sample();
        assert_eq!(
            v.set(&p("nope.deeper"), Value::Null),
            Err(SetError::MissingKey(p("nope.deeper")))
        );
    }

    #[test]
    fn set_out_of_bounds_index_errors() {
        let mut v = sample();
        assert_eq!(
            v.set(&p("documents[9]"), Value::Null),
            Err(SetError::IndexOutOfBounds(9))
        );
    }

    #[test]
    fn set_wrong_kind_errors() {
        let mut v = sample();
        // `editor` is a map; indexing into it is a type error.
        assert_eq!(
            v.set(&p("editor[0]"), Value::Null),
            Err(SetError::TypeMismatch(p("editor[0]")))
        );
    }

    #[test]
    fn set_through_out_of_bounds_intermediate_index_errors() {
        let mut v = sample();
        // The failing segment is intermediate, not final.
        assert_eq!(
            v.set(&p("documents[9].title"), Value::Null),
            Err(SetError::IndexOutOfBounds(9))
        );
    }

    #[test]
    fn out_of_bounds_error_reports_the_offending_index_not_the_length() {
        let mut v = sample();
        // Distinguishes a correct implementation from one reporting len().
        assert_eq!(
            v.set(&p("documents[9]"), Value::Null),
            Err(SetError::IndexOutOfBounds(9))
        );
        assert_eq!(
            v.set(&p("documents[42]"), Value::Null),
            Err(SetError::IndexOutOfBounds(42))
        );
    }

    #[test]
    fn set_and_get_non_zero_list_index_leaves_index_zero_unchanged() {
        let mut v = sample();
        // `documents` now has 3 elements; this kills a mutant that always
        // reads or writes slot 0 regardless of the requested index.
        v.set(&p("documents[2].title"), Value::Str("edited".into()))
            .unwrap();
        assert_eq!(
            v.get(&p("documents[2].title")),
            Some(&Value::Str("edited".into()))
        );
        assert_eq!(
            v.get(&p("documents[0].title")),
            Some(&Value::Str("first".into())),
            "writing index 2 must not disturb index 0"
        );
    }

    // --- Required additions beyond the plan's example-based tests ---

    /// Recursively collects every path `get` can reach in `value`, including
    /// the root and every intermediate node, not just leaves. Used by the
    /// round-trip property test below.
    fn collect_paths(value: &Value, prefix: &Path, out: &mut Vec<Path>) {
        out.push(prefix.clone());
        match value {
            Value::Map(m) => {
                for (k, child) in m {
                    let mut segments = prefix.segments().to_vec();
                    segments.push(Segment::Key(k.clone()));
                    collect_paths(child, &Path::from_segments(segments), out);
                }
            }
            Value::List(l) => {
                for (i, child) in l.iter().enumerate() {
                    let mut segments = prefix.segments().to_vec();
                    segments.push(Segment::Index(i));
                    collect_paths(child, &Path::from_segments(segments), out);
                }
            }
            _ => {}
        }
    }

    /// For every path that `get` can reach, writing a value there and reading
    /// it back must return exactly that value, and every *other* reachable
    /// path that does not overlap it must retain its original value. This is
    /// (A) from the task spec, tightened by review round two into a genuine
    /// frame condition ("this write changed exactly one thing and nothing
    /// else") rather than a single opposite-branch canary, which could not
    /// detect a mutant that clobbers same-map siblings at depth >= 2 (e.g.
    /// writing `editor.zoom` silently changing `editor.theme`, two leaves of
    /// the *same* map). Per the standing quality bar, property tests pin
    /// structure and example tests pin semantics, and a prior task in this
    /// crate found that property tests alone can pass against a mutant that
    /// breaks semantics.
    #[test]
    fn set_then_get_round_trips_at_every_reachable_path() {
        let pristine = sample();
        let paths = {
            let mut out = Vec::new();
            collect_paths(&pristine, &Path::root(), &mut out);
            out
        };

        // `sample()` has exactly 20 reachable nodes:
        //   root                                             1
        //   editor, editor.theme, editor.zoom                3
        //   documents, documents[i], documents[i].title (i=0..=2)  1 + 3*2 = 7
        //   matrix, matrix[0], matrix[0][0], matrix[0][1],
        //     matrix[1], matrix[1][0]                        6
        //   empty_map                                        1
        //   empty_list                                       1
        //   thumbnail                                        1
        //                                             total = 20
        // Pinned exactly so a future change to `sample()` that silently
        // drops a branch (and with it a class of paths this test exercises)
        // is caught here rather than discovered later. If this number
        // changes, recount deliberately using the breakdown above rather
        // than just updating the digit.
        assert_eq!(paths.len(), 20, "reachable path count changed in sample()");

        // Snapshot every reachable path's original value once, up front, so
        // each iteration below can check every *other* path against it.
        let snapshot: Vec<(Path, Value)> = paths
            .iter()
            .map(|path| {
                let value = pristine
                    .get(path)
                    .unwrap_or_else(|| panic!("collected path {path} must be reachable"))
                    .clone();
                (path.clone(), value)
            })
            .collect();

        for path in &paths {
            let mut v = pristine.clone();
            let sentinel = Value::Str("SENTINEL".to_string());
            v.set(path, sentinel.clone())
                .unwrap_or_else(|e| panic!("set at reachable path {path} must succeed: {e}"));
            assert_eq!(
                v.get(path),
                Some(&sentinel),
                "round trip failed for path {path}"
            );

            // Frame condition: every other reachable path that does not
            // overlap `path` (neither an ancestor nor a descendant of it)
            // must retain exactly its original `sample()` value. Ancestors
            // and descendants are excluded because writing `path` legitimately
            // changes them (an ancestor's subtree now contains the sentinel;
            // a descendant no longer exists once `path` becomes a scalar).
            for (other_path, original_value) in &snapshot {
                if path.overlaps(other_path) {
                    continue;
                }
                assert_eq!(
                    v.get(other_path),
                    Some(original_value),
                    "writing {path} disturbed unrelated path {other_path}"
                );
            }
        }
    }

    // (B) Error-path tests the plan's example tests miss.

    #[test]
    fn set_through_a_scalar_is_type_mismatch_not_panic() {
        let mut v = sample();
        // `editor.zoom` is a `Float`; descending further into it must error,
        // not panic.
        assert_eq!(
            v.set(&p("editor.zoom.deeper"), Value::Null),
            Err(SetError::TypeMismatch(p("editor.zoom.deeper")))
        );
    }

    #[test]
    fn get_through_a_scalar_returns_none_not_panic() {
        assert_eq!(sample().get(&p("editor.zoom.deeper")), None);
    }

    #[test]
    fn set_final_key_against_list_errors() {
        let mut v = sample();
        // `documents` is a `List`; a key segment against it is a type error,
        // even as the final segment.
        assert_eq!(
            v.set(&p("documents.title"), Value::Null),
            Err(SetError::TypeMismatch(p("documents.title")))
        );
    }

    #[test]
    fn set_intermediate_index_against_map_errors() {
        let mut v = sample();
        // `editor` is a `Map`; an index segment against it is a type error
        // when it is an intermediate segment, not just when final.
        assert_eq!(
            v.set(&p("editor[0].nested"), Value::Null),
            Err(SetError::TypeMismatch(p("editor[0].nested")))
        );
    }

    #[test]
    fn set_intermediate_key_against_list_errors() {
        let mut v = sample();
        // `documents` is a `List`; a key segment against it is a type error
        // when it is an intermediate segment, not just when final.
        assert_eq!(
            v.set(&p("documents.foo.bar"), Value::Null),
            Err(SetError::TypeMismatch(p("documents.foo.bar")))
        );
    }

    #[test]
    fn set_index_one_past_end_is_out_of_bounds_not_append() {
        let mut v = sample();
        // `documents` has length 3 (valid indices: 0, 1, 2). Index 3 is one
        // past the end and must error, not silently grow the list.
        assert_eq!(
            v.set(&p("documents[3]"), Value::Null),
            Err(SetError::IndexOutOfBounds(3))
        );
        assert_eq!(v, sample(), "a rejected append must not mutate the list");
    }

    // (C) A failed write must leave the tree completely untouched: `Store`
    // will rely on this to guarantee no notification and no partial mutation
    // on a failed write.

    #[test]
    fn failed_writes_leave_the_tree_completely_untouched() {
        let failing_paths_and_values: [(Path, Value); 8] = [
            (p("nope.deeper"), Value::Null),
            (p("editor[0]"), Value::Null),
            (p("documents[9]"), Value::Null),
            (p("editor.zoom.deeper"), Value::Null),
            (p("documents.title"), Value::Null),
            (p("editor[0].nested"), Value::Null),
            (p("documents[3]"), Value::Null),
            (p("documents[9].title"), Value::Null),
        ];

        for (path, value) in failing_paths_and_values {
            let mut v = sample();
            let result = v.set(&path, value);
            assert!(
                result.is_err(),
                "expected {path} to fail so the untouched-tree guarantee is actually exercised"
            );
            assert_eq!(
                v,
                sample(),
                "failed set at {path} must not mutate the tree at all"
            );
        }
    }

    // (D) `Value::map` with an empty iterator.

    #[test]
    fn map_with_empty_iterator_produces_an_empty_map() {
        // `Value::map([])` compiles without a turbofish here because the
        // `let` binding's `Value` type, combined with the single `Value::Map`
        // variant `map` can construct, is enough for inference to pick
        // `entries: []` as `[(&str, Value); 0]`. A call site without any
        // surrounding type context (e.g. passed directly to a function
        // generic over its argument) would need an explicit turbofish, e.g.
        // `Value::map::<&str, _>([])`.
        let v: Value = Value::map([]);
        match v {
            Value::Map(m) => assert_eq!(m.len(), 0),
            other => panic!("expected an empty Value::Map, got {other:?}"),
        }
    }

    // (E) NaN breaks reflexivity of derived `PartialEq`. Documented on the
    // `Float` variant; pinned here, not "fixed" with a hand-written impl.

    #[test]
    fn float_nan_is_not_equal_to_itself() {
        let a = Value::Float(f64::NAN);
        let b = Value::Float(f64::NAN);
        #[allow(clippy::eq_op)]
        {
            assert_ne!(a, b, "IEEE 754 NaN is never equal to NaN, including itself");
        }
    }
}
