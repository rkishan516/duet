# Phase 1 — `duet-core` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the platform-free heart of Duet — an observable state store with
path-scoped subscriptions and minimal patches, a surface lifecycle state machine, and pure
policy evaluation — with no native dependencies and 90%+ test coverage.

**Architecture:** `duet-core` operates on a **dynamic `Value` tree**, not on user generic
types. Typed accessors are generated in Phase 4 and layer on top. This keeps Phase 1 free of
proc-macro machinery and makes every behaviour directly unit-testable. Every public
operation is a pure function or returns its effects as data (`set` returns
`Vec<Notification>` rather than invoking callbacks), so nothing needs mocking, threading, or
a clock.

**Tech Stack:** Rust 1.92, edition 2024, **zero runtime dependencies**. `cargo-llvm-cov`
for coverage.

**Reference:** `docs/superpowers/specs/2026-08-04-duet-design.md` §4 (state), §5 (lifecycle).

---

## Background for the implementer

You need three concepts from the spec. Read them here; you do not need the whole spec.

**1. Paths address into the state tree.** `editor.zoom` and `documents[3].title` are paths.
A path is a list of segments, each either a map key or a list index. The empty path is the
root.

**2. Subscription matching uses two-way prefix overlap.** A subscriber at path `P` is
notified about a write at path `W` when *either path is a prefix of the other*. Writing
`editor.zoom` notifies subscribers at `editor.zoom`, `editor`, and root — but not at
`editor.theme`. Writing `editor` (the whole struct) notifies a subscriber at `editor.zoom`,
because their value may have changed. The two-way rule is the entire subtlety here; get it
right and everything else follows.

**3. Patches carry the written path, not the subscriber's path.** When `editor.zoom` is
written, *every* matching subscriber receives the same patch `(editor.zoom, 1.5)` — even a
subscriber watching root. Clients keep a local mirror and apply the patch to it. This is why
a subscriber on a 10,000-item list receives one string instead of 10,000 items when one item
changes.

**Time is never read from the system.** `Instant(u64)` is monotonic milliseconds supplied by
the caller. Tests pass literal numbers. There is no clock to mock.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` (root) | Workspace definition |
| `crates/duet-core/Cargo.toml` | Crate manifest, no dependencies |
| `crates/duet-core/src/lib.rs` | Module declarations and public re-exports |
| `crates/duet-core/src/value.rs` | `Value` enum, `get`/`set` by path, `SetError` |
| `crates/duet-core/src/path.rs` | `Segment`, `Path`, parsing, `Display`, prefix matching |
| `crates/duet-core/src/store.rs` | `Store`, subscriptions, `Patch`, `Notification` |
| `crates/duet-core/src/lifecycle.rs` | `Instant`, `SurfaceState`, `LifecycleEvent`, `transition` |
| `crates/duet-core/src/policy.rs` | `Policy`, `PolicyInput`, `Decision`, `evaluate` |

Tests live in `#[cfg(test)] mod tests` at the bottom of each file — the Rust convention, and
it keeps behaviour next to the code that implements it.

---

## Task 1: Workspace scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `crates/duet-core/Cargo.toml`
- Create: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Create the workspace manifest**

Create `Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/duet-core"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
repository = "https://github.com/rkishan516/tauri-flutter"
```

- [ ] **Step 2: Create the crate manifest**

Create `crates/duet-core/Cargo.toml`:

```toml
[package]
name = "duet-core"
description = "Platform-free state store, lifecycle, and policy engine for Duet"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
```

The empty `[dependencies]` is deliberate. If you find yourself adding one in this phase,
stop and reconsider — this crate must compile and test on any machine with only a Rust
toolchain.

- [ ] **Step 3: Create the crate root**

Create `crates/duet-core/src/lib.rs`:

```rust
//! Platform-free core of the Duet framework.
//!
//! Contains the observable state store, the surface lifecycle state machine, and
//! policy evaluation. This crate has no platform dependencies and no runtime
//! dependencies, so all behaviour here is testable with plain `cargo test`.

pub mod path;

pub use path::{Path, PathParseError, Segment};
```

- [ ] **Step 4: Create an empty path module so the crate compiles**

Create `crates/duet-core/src/path.rs`:

```rust
//! Paths addressing into the state tree.
```

This will not compile yet — `lib.rs` re-exports names that do not exist. That is expected;
Task 2 defines them. Skip straight to Task 2 rather than trying to build.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/
git commit -m "chore: scaffold duet-core workspace"
```

---

## Task 2: `Path` and `Segment` types

**Files:**
- Modify: `crates/duet-core/src/path.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/duet-core/src/path.rs`:

```rust
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
        let p = Path::from_segments(vec![
            Segment::Key("editor".to_string()),
            Segment::Key("zoom".to_string()),
        ]);
        assert!(!p.is_root());
        assert_eq!(p.segments().len(), 2);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — compilation errors, `cannot find type Path in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert into `crates/duet-core/src/path.rs`, above the `#[cfg(test)]` block:

```rust
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
    pub fn root() -> Self {
        Path(Vec::new())
    }

    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Path(segments)
    }

    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}
```

Also add a placeholder so the `lib.rs` re-export compiles:

```rust
/// Reasons a path string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathParseError {
    EmptySegment(usize),
    UnclosedIndex(usize),
    InvalidIndex(String),
    TrailingDot,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — `test result: ok. 2 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/path.rs
git commit -m "feat(core): add Path and Segment types"
```

---

## Task 3: Path parsing and display

**Files:**
- Modify: `crates/duet-core/src/path.rs`

- [ ] **Step 1: Write the failing test**

Add these tests inside the existing `mod tests` block in `crates/duet-core/src/path.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `no function or associated item named parse found`.

- [ ] **Step 3: Write minimal implementation**

Add to the `impl Path` block in `crates/duet-core/src/path.rs`:

```rust
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
            } else {
                let mut end = i;
                while end < bytes.len() && bytes[end] != b'.' && bytes[end] != b'[' {
                    end += 1;
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
```

Add the `Display` impl at the end of the file, outside `impl Path` and outside the test
module:

```rust
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
```

Note why `Display` does not simply join with `.`: an index segment must render as `[3]` with
no preceding dot, so `documents[3].title` round-trips. The `first` flag plus the
index-writes-no-dot rule is what makes the round-trip test pass.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 10 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/path.rs
git commit -m "feat(core): parse and display paths"
```

---

## Task 4: Prefix matching

This is the subscription-matching rule from spec §4.2. It is small and load-bearing.

**Files:**
- Modify: `crates/duet-core/src/path.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/duet-core/src/path.rs`:

```rust
    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `no method named is_prefix_of`.

- [ ] **Step 3: Write minimal implementation**

Add to the `impl Path` block:

```rust
    /// True when `self` addresses this path or any ancestor of `other`.
    pub fn is_prefix_of(&self, other: &Path) -> bool {
        self.0.len() <= other.0.len()
            && self.0.iter().zip(other.0.iter()).all(|(a, b)| a == b)
    }

    /// True when either path is a prefix of the other.
    ///
    /// This is the subscription-matching rule: a subscriber at `self` must be
    /// notified about a write at `other` when the paths overlap in either
    /// direction. A write to an ancestor may change the subscriber's value, and a
    /// write to a descendant changes part of the subtree the subscriber observes.
    pub fn overlaps(&self, other: &Path) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 16 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/path.rs
git commit -m "feat(core): add two-way prefix matching for subscriptions"
```

---

## Task 5: `Value` type and reads

**Files:**
- Create: `crates/duet-core/src/value.rs`
- Modify: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-core/src/value.rs`:

```rust
//! The dynamic state tree.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;

    fn sample() -> Value {
        Value::map([
            (
                "editor",
                Value::map([("zoom", Value::Float(1.0)), ("theme", Value::Str("dark".into()))]),
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `cannot find type Value in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert above the `#[cfg(test)]` block in `crates/duet-core/src/value.rs`:

```rust
use std::collections::BTreeMap;

use crate::path::{Path, Segment};

/// A dynamically typed node in the state tree.
///
/// Typed access is generated in Phase 4 and layers on top of this. Keeping the
/// runtime representation dynamic is what allows path addressing and minimal
/// patches to work without any knowledge of user types.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    /// `BTreeMap` rather than `HashMap`: deterministic ordering keeps patch
    /// payloads and golden-file tests stable.
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// Convenience constructor for map literals in tests and app setup.
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
```

Update `crates/duet-core/src/lib.rs` to declare and re-export the module:

```rust
//! Platform-free core of the Duet framework.
//!
//! Contains the observable state store, the surface lifecycle state machine, and
//! policy evaluation. This crate has no platform dependencies and no runtime
//! dependencies, so all behaviour here is testable with plain `cargo test`.

pub mod path;
pub mod value;

pub use path::{Path, PathParseError, Segment};
pub use value::Value;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 22 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/value.rs crates/duet-core/src/lib.rs
git commit -m "feat(core): add Value tree with path reads"
```

---

## Task 6: `Value` writes

**Files:**
- Modify: `crates/duet-core/src/value.rs`
- Modify: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/duet-core/src/value.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `no method named set`, `cannot find type SetError`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/duet-core/src/value.rs`, above the test module:

```rust
/// Why a write could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    /// An intermediate map key on the way to the target does not exist.
    MissingKey(Path),
    /// A list index on the way to the target is out of bounds.
    IndexOutOfBounds(usize),
    /// A segment addressed the wrong kind of node, e.g. indexing into a map.
    TypeMismatch(Path),
}
```

Add to the `impl Value` block:

```rust
    /// Writes `value` at `path`.
    ///
    /// Intermediate nodes are never created: writing to `a.b` when `a` does not
    /// exist is a `MissingKey` error rather than an implicit insert. The final
    /// segment of a map path *is* inserted if absent, so adding a new key to an
    /// existing map succeeds.
    pub fn set(&mut self, path: &Path, value: Value) -> Result<(), SetError> {
        let segments = path.segments();
        let Some((last, parents)) = segments.split_last() else {
            *self = value;
            return Ok(());
        };

        let mut current: &mut Value = self;
        for segment in parents {
            current = match (current, segment) {
                (Value::Map(m), Segment::Key(k)) => {
                    m.get_mut(k).ok_or_else(|| SetError::MissingKey(path.clone()))?
                }
                (Value::List(l), Segment::Index(i)) => {
                    l.get_mut(*i).ok_or(SetError::IndexOutOfBounds(*i))?
                }
                _ => return Err(SetError::TypeMismatch(path.clone())),
            };
        }

        match (current, last) {
            (Value::Map(m), Segment::Key(k)) => {
                m.insert(k.clone(), value);
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
```

The `split_last` handles the root case: an empty segment list means replace the whole tree.
Descending through `parents` and handling `last` separately is what allows the final map key
to be inserted while intermediate keys must already exist.

Update the re-exports in `crates/duet-core/src/lib.rs`:

```rust
pub use value::{SetError, Value};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 29 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/value.rs crates/duet-core/src/lib.rs
git commit -m "feat(core): add Value writes with typed errors"
```

---

## Task 7: `Store` subscriptions and snapshots

**Files:**
- Create: `crates/duet-core/src/store.rs`
- Modify: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-core/src/store.rs`:

```rust
//! The observable state store.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;
    use crate::value::Value;

    fn sample() -> Value {
        Value::map([(
            "editor",
            Value::map([("zoom", Value::Float(1.0)), ("theme", Value::Str("dark".into()))]),
        )])
    }

    fn p(s: &str) -> Path {
        Path::parse(s).expect("test path should parse")
    }

    #[test]
    fn subscribe_returns_snapshot_of_that_path() {
        let mut store = Store::new(sample());
        let (_id, snapshot) = store.subscribe(SubscriberId(1), p("editor.zoom"));
        assert_eq!(snapshot, Some(Value::Float(1.0)));
    }

    #[test]
    fn subscribe_to_missing_path_returns_no_snapshot() {
        let mut store = Store::new(sample());
        let (_id, snapshot) = store.subscribe(SubscriberId(1), p("editor.nope"));
        assert_eq!(snapshot, None);
    }

    #[test]
    fn subscription_ids_are_unique() {
        let mut store = Store::new(sample());
        let (a, _) = store.subscribe(SubscriberId(1), Path::root());
        let (b, _) = store.subscribe(SubscriberId(1), Path::root());
        assert_ne!(a, b);
    }

    #[test]
    fn unsubscribe_removes_the_subscription() {
        let mut store = Store::new(sample());
        let (id, _) = store.subscribe(SubscriberId(1), Path::root());
        assert!(store.unsubscribe(&id));
        assert!(!store.unsubscribe(&id), "second removal should report false");
    }

    #[test]
    fn get_reads_through_to_the_tree() {
        let store = Store::new(sample());
        assert_eq!(store.get(&p("editor.theme")), Some(&Value::Str("dark".into())));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `cannot find type Store in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `crates/duet-core/src/store.rs`:

```rust
use crate::path::Path;
use crate::value::{SetError, Value};

/// Identifies a guest that holds subscriptions, e.g. a Flutter or webview surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberId(pub u64);

/// Identifies one subscription held by a subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub u64);

#[derive(Debug, Clone)]
struct Subscription {
    id: SubscriptionId,
    subscriber: SubscriberId,
    path: Path,
}

/// The authoritative state tree plus its subscription registry.
#[derive(Debug, Clone)]
pub struct Store {
    root: Value,
    subscriptions: Vec<Subscription>,
    next_id: u64,
}

impl Store {
    pub fn new(root: Value) -> Self {
        Store {
            root,
            subscriptions: Vec::new(),
            next_id: 0,
        }
    }

    pub fn get(&self, path: &Path) -> Option<&Value> {
        self.root.get(path)
    }

    /// Registers a subscription and returns its id plus the current value at
    /// `path`, if any.
    ///
    /// The snapshot is why resume-from-`Cold` needs no special path: a guest that
    /// restarts simply subscribes again and receives current state.
    pub fn subscribe(&mut self, subscriber: SubscriberId, path: Path) -> (SubscriptionId, Option<Value>) {
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        let snapshot = self.root.get(&path).cloned();
        self.subscriptions.push(Subscription {
            id,
            subscriber,
            path,
        });
        (id, snapshot)
    }

    /// Removes a subscription. Returns whether it was present.
    pub fn unsubscribe(&mut self, id: &SubscriptionId) -> bool {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| &s.id != id);
        self.subscriptions.len() != before
    }

    /// Removes every subscription held by `subscriber`, returning how many were
    /// removed. Called when a surface goes `Cold`.
    pub fn drop_subscriber(&mut self, subscriber: SubscriberId) -> usize {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| s.subscriber != subscriber);
        before - self.subscriptions.len()
    }
}
```

Update `crates/duet-core/src/lib.rs`:

```rust
pub mod path;
pub mod store;
pub mod value;

pub use path::{Path, PathParseError, Segment};
pub use store::{Store, SubscriberId, SubscriptionId};
pub use value::{SetError, Value};
```

The unused `SetError` import in `store.rs` is intentional — Task 8 uses it. If the compiler
warns, leave it; the next task resolves it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 34 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/store.rs crates/duet-core/src/lib.rs
git commit -m "feat(core): add Store with subscriptions and snapshots"
```

---

## Task 8: Writes produce notifications

This is the centre of the crate. Read the "Background" section again before starting if the
two-way overlap rule is not fresh.

**Files:**
- Modify: `crates/duet-core/src/store.rs`
- Modify: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/duet-core/src/store.rs`:

```rust
    #[test]
    fn write_notifies_exact_path_subscriber() {
        let mut store = Store::new(sample());
        let (id, _) = store.subscribe(SubscriberId(1), p("editor.zoom"));

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscription, id);
        assert_eq!(notes[0].subscriber, SubscriberId(1));
        assert_eq!(notes[0].patch.path, p("editor.zoom"));
        assert_eq!(notes[0].patch.value, Value::Float(2.0));
    }

    #[test]
    fn write_notifies_ancestor_subscribers() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor"));
        store.subscribe(SubscriberId(2), Path::root());

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn write_notifies_descendant_subscribers() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor.zoom"));

        // Replacing the whole `editor` struct may change `editor.zoom`.
        let notes = store
            .set(&p("editor"), Value::map([("zoom", Value::Float(9.0))]))
            .unwrap();

        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn write_does_not_notify_siblings() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), p("editor.theme"));

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        assert!(notes.is_empty());
    }

    #[test]
    fn patch_carries_written_path_not_subscriber_path() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), Path::root());

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();

        // A root subscriber receives the narrow patch, not the whole tree.
        assert_eq!(notes[0].patch.path, p("editor.zoom"));
        assert_eq!(notes[0].patch.value, Value::Float(2.0));
    }

    #[test]
    fn failed_write_notifies_nobody_and_leaves_state_intact() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), Path::root());

        let result = store.set(&p("nope.deeper"), Value::Null);

        assert!(result.is_err());
        assert_eq!(store.get(&p("editor.zoom")), Some(&Value::Float(1.0)));
    }

    #[test]
    fn dropped_subscriber_stops_receiving_notifications() {
        let mut store = Store::new(sample());
        store.subscribe(SubscriberId(1), Path::root());
        store.subscribe(SubscriberId(2), Path::root());

        assert_eq!(store.drop_subscriber(SubscriberId(1)), 1);

        let notes = store.set(&p("editor.zoom"), Value::Float(2.0)).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].subscriber, SubscriberId(2));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `no method named set found for struct Store`.

- [ ] **Step 3: Write minimal implementation**

Add above the `Store` struct in `crates/duet-core/src/store.rs`:

```rust
/// A minimal change: the path that was written and its new value.
///
/// Every matching subscriber receives the same patch regardless of where they
/// subscribed. Clients apply it to a local mirror. This is what keeps a
/// subscriber on a large collection from receiving the whole collection when one
/// element changes.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub path: Path,
    pub value: Value,
}

/// A patch addressed to one subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub subscriber: SubscriberId,
    pub subscription: SubscriptionId,
    pub patch: Patch,
}
```

Add to the `impl Store` block:

```rust
    /// Applies a write and returns the notifications it produced.
    ///
    /// Returning effects as data rather than invoking callbacks keeps this
    /// method pure enough to test directly, and lets the caller decide which
    /// thread each notification is delivered on.
    ///
    /// On error nothing is mutated and no notifications are produced.
    pub fn set(&mut self, path: &Path, value: Value) -> Result<Vec<Notification>, SetError> {
        self.root.set(path, value.clone())?;

        let patch = Patch {
            path: path.clone(),
            value,
        };

        Ok(self
            .subscriptions
            .iter()
            .filter(|s| s.path.overlaps(path))
            .map(|s| Notification {
                subscriber: s.subscriber,
                subscription: s.id,
                patch: patch.clone(),
            })
            .collect())
    }
```

Update `crates/duet-core/src/lib.rs`:

```rust
pub use store::{Notification, Patch, Store, SubscriberId, SubscriptionId};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 41 passed.

- [ ] **Step 5: Verify there are no warnings**

Run: `cargo clippy -p duet-core --all-targets -- -D warnings`
Expected: no output, exit code 0. If `SetError` is reported as an unused import, it is now
used by `set`; if clippy still complains, remove whichever import it names.

- [ ] **Step 6: Commit**

```bash
git add crates/duet-core/src/store.rs crates/duet-core/src/lib.rs
git commit -m "feat(core): produce minimal patches on write"
```

---

## Task 9: Lifecycle state machine

**Files:**
- Create: `crates/duet-core/src/lifecycle.rs`
- Modify: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-core/src/lifecycle.rs`:

```rust
//! Surface lifecycle state machine.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_starts() {
        assert_eq!(
            transition(&SurfaceState::Cold, &LifecycleEvent::Start),
            Ok(SurfaceState::Starting)
        );
    }

    #[test]
    fn starting_becomes_live_when_ready() {
        assert_eq!(
            transition(&SurfaceState::Starting, &LifecycleEvent::Ready),
            Ok(SurfaceState::Live)
        );
    }

    #[test]
    fn live_suspends_with_timestamp() {
        assert_eq!(
            transition(
                &SurfaceState::Live,
                &LifecycleEvent::Suspend { at: Instant(1_000) }
            ),
            Ok(SurfaceState::Suspending {
                since: Instant(1_000)
            })
        );
    }

    #[test]
    fn resume_during_grace_cancels_teardown() {
        // The anti-thrash property: reopening during the grace window returns
        // straight to Live without paying an engine boot.
        assert_eq!(
            transition(
                &SurfaceState::Suspending {
                    since: Instant(1_000)
                },
                &LifecycleEvent::Resume
            ),
            Ok(SurfaceState::Live)
        );
    }

    #[test]
    fn grace_expiry_goes_cold() {
        assert_eq!(
            transition(
                &SurfaceState::Suspending {
                    since: Instant(1_000)
                },
                &LifecycleEvent::GraceExpired
            ),
            Ok(SurfaceState::Cold)
        );
    }

    #[test]
    fn cold_resumes_by_starting() {
        assert_eq!(
            transition(&SurfaceState::Cold, &LifecycleEvent::Resume),
            Ok(SurfaceState::Starting)
        );
    }

    #[test]
    fn failure_is_reachable_from_any_state() {
        for state in [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Live,
            SurfaceState::Suspending { since: Instant(0) },
        ] {
            assert_eq!(
                transition(&state, &LifecycleEvent::Fail("boom".into())),
                Ok(SurfaceState::Failed("boom".into())),
                "failure should be reachable from {state:?}"
            );
        }
    }

    #[test]
    fn failed_retries_by_starting() {
        assert_eq!(
            transition(&SurfaceState::Failed("boom".into()), &LifecycleEvent::Retry),
            Ok(SurfaceState::Starting)
        );
    }

    #[test]
    fn invalid_transition_is_rejected() {
        assert_eq!(
            transition(&SurfaceState::Live, &LifecycleEvent::Ready),
            Err(InvalidTransition {
                from: SurfaceState::Live,
                event: LifecycleEvent::Ready,
            })
        );
    }

    #[test]
    fn retry_is_only_valid_from_failed() {
        assert!(transition(&SurfaceState::Live, &LifecycleEvent::Retry).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `cannot find function transition in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `crates/duet-core/src/lifecycle.rs`:

```rust
/// Monotonic milliseconds, supplied by the caller.
///
/// The core never reads a system clock. Callers pass `now`, which makes every
/// time-dependent behaviour deterministic in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(pub u64);

/// Lifecycle state of one surface. See spec §5.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceState {
    /// No engine, no webview, no renderer process. The store retains everything.
    Cold,
    /// Engine booting or webview creating. Requests are queued.
    Starting,
    /// Attached, rendering, receiving events.
    Live,
    /// Grace period. A resume here cancels teardown.
    Suspending { since: Instant },
    /// Creation failed or the guest crashed. The host stays alive.
    Failed(String),
}

/// Inputs that drive the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEvent {
    Start,
    Ready,
    Suspend { at: Instant },
    Resume,
    GraceExpired,
    Fail(String),
    Retry,
}

/// Returned when an event does not apply to the current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: SurfaceState,
    pub event: LifecycleEvent,
}

/// Computes the next state. Pure — no side effects, no clock.
pub fn transition(
    from: &SurfaceState,
    event: &LifecycleEvent,
) -> Result<SurfaceState, InvalidTransition> {
    use LifecycleEvent as E;
    use SurfaceState as S;

    let next = match (from, event) {
        // Failure interrupts any state, so it is matched first.
        (_, E::Fail(why)) => S::Failed(why.clone()),

        (S::Cold, E::Start) => S::Starting,
        (S::Cold, E::Resume) => S::Starting,
        (S::Starting, E::Ready) => S::Live,
        (S::Live, E::Suspend { at }) => S::Suspending { since: *at },
        (S::Suspending { .. }, E::Resume) => S::Live,
        (S::Suspending { .. }, E::GraceExpired) => S::Cold,
        (S::Failed(_), E::Retry) => S::Starting,

        _ => {
            return Err(InvalidTransition {
                from: from.clone(),
                event: event.clone(),
            });
        }
    };

    Ok(next)
}
```

Ordering matters: the `Fail` arm is first so failure is reachable from every state without
enumerating each one.

Update `crates/duet-core/src/lib.rs`:

```rust
pub mod lifecycle;
pub mod path;
pub mod store;
pub mod value;

pub use lifecycle::{Instant, InvalidTransition, LifecycleEvent, SurfaceState, transition};
pub use path::{Path, PathParseError, Segment};
pub use store::{Notification, Patch, Store, SubscriberId, SubscriptionId};
pub use value::{SetError, Value};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 51 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/lifecycle.rs crates/duet-core/src/lib.rs
git commit -m "feat(core): add surface lifecycle state machine"
```

---

## Task 10: Policy evaluation

**Files:**
- Create: `crates/duet-core/src/policy.rs`
- Modify: `crates/duet-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-core/src/policy.rs`:

```rust
//! Teardown policy evaluation.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::{Instant, SurfaceState};

    fn live(open: usize, visible: usize) -> PolicyInput {
        PolicyInput {
            state: SurfaceState::Live,
            open_windows: open,
            visible_windows: visible,
            last_interaction: Instant(0),
            now: Instant(0),
        }
    }

    #[test]
    fn default_policy_is_five_second_grace_on_last_window_closed() {
        assert_eq!(
            Policy::default(),
            Policy::OnLastWindowClosed { grace_ms: 5_000 }
        );
    }

    #[test]
    fn never_policy_never_suspends() {
        assert_eq!(evaluate(&Policy::Never, &live(0, 0)), Decision::NoChange);
    }

    #[test]
    fn last_window_closed_suspends_at_zero_open_windows() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };
        assert_eq!(evaluate(&policy, &live(1, 0)), Decision::NoChange);
        assert_eq!(evaluate(&policy, &live(0, 0)), Decision::Suspend);
    }

    #[test]
    fn on_hidden_ignores_open_count_and_watches_visibility() {
        let policy = Policy::OnHidden { grace_ms: 1_000 };
        // Window is open but hidden: suspend.
        assert_eq!(evaluate(&policy, &live(1, 0)), Decision::Suspend);
        assert_eq!(evaluate(&policy, &live(1, 1)), Decision::NoChange);
    }

    #[test]
    fn idle_timeout_suspends_only_after_the_interval() {
        let policy = Policy::IdleTimeout { after_ms: 1_000 };
        let mut input = live(1, 1);

        input.last_interaction = Instant(0);
        input.now = Instant(999);
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);

        input.now = Instant(1_000);
        assert_eq!(evaluate(&policy, &input), Decision::Suspend);
    }

    #[test]
    fn suspending_tears_down_only_after_grace_elapses() {
        let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending {
            since: Instant(1_000),
        };

        input.now = Instant(5_999);
        assert_eq!(evaluate(&policy, &input), Decision::NoChange);

        input.now = Instant(6_000);
        assert_eq!(evaluate(&policy, &input), Decision::Teardown);
    }

    #[test]
    fn never_policy_does_not_tear_down_even_while_suspending() {
        let mut input = live(0, 0);
        input.state = SurfaceState::Suspending { since: Instant(0) };
        input.now = Instant(u64::MAX);
        assert_eq!(evaluate(&Policy::Never, &input), Decision::NoChange);
    }

    #[test]
    fn idle_timeout_tears_down_immediately_once_suspending() {
        let mut input = live(1, 1);
        input.state = SurfaceState::Suspending { since: Instant(10) };
        input.now = Instant(10);
        assert_eq!(
            evaluate(&Policy::IdleTimeout { after_ms: 1_000 }, &input),
            Decision::Teardown
        );
    }

    #[test]
    fn non_live_non_suspending_states_are_left_alone() {
        for state in [
            SurfaceState::Cold,
            SurfaceState::Starting,
            SurfaceState::Failed("boom".into()),
        ] {
            let mut input = live(0, 0);
            input.state = state.clone();
            assert_eq!(
                evaluate(&Policy::OnLastWindowClosed { grace_ms: 0 }, &input),
                Decision::NoChange,
                "state {state:?} should be left alone"
            );
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core`
Expected: FAIL — `cannot find type Policy in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `crates/duet-core/src/policy.rs`:

```rust
use crate::lifecycle::{Instant, SurfaceState};

/// When a surface's resources should be released. See spec §5.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    /// Suspend once every window for this surface is closed.
    OnLastWindowClosed { grace_ms: u64 },
    /// Suspend once every window for this surface is hidden, even if still open.
    OnHidden { grace_ms: u64 },
    /// Suspend after this long without interaction.
    IdleTimeout { after_ms: u64 },
    /// Never suspend automatically.
    Never,
}

impl Default for Policy {
    fn default() -> Self {
        Policy::OnLastWindowClosed { grace_ms: 5_000 }
    }
}

/// Everything policy evaluation is allowed to look at.
///
/// `now` is passed in rather than read, so evaluation stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInput {
    pub state: SurfaceState,
    pub open_windows: usize,
    pub visible_windows: usize,
    /// Last input event, command, or store write from this surface.
    pub last_interaction: Instant,
    pub now: Instant,
}

/// What the caller should do with the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    NoChange,
    /// Move `Live` -> `Suspending`.
    Suspend,
    /// Move `Suspending` -> `Cold`.
    Teardown,
}

/// Decides what should happen to a surface. Pure — no side effects, no clock.
pub fn evaluate(policy: &Policy, input: &PolicyInput) -> Decision {
    // While suspending, the only question is whether the grace period elapsed.
    if let SurfaceState::Suspending { since } = input.state {
        let grace_ms = match policy {
            Policy::Never => return Decision::NoChange,
            Policy::OnLastWindowClosed { grace_ms } | Policy::OnHidden { grace_ms } => *grace_ms,
            // Idle timeout has no separate grace: the idle interval was the grace.
            Policy::IdleTimeout { .. } => 0,
        };

        return if input.now.0.saturating_sub(since.0) >= grace_ms {
            Decision::Teardown
        } else {
            Decision::NoChange
        };
    }

    // Only a live surface can begin suspending.
    if input.state != SurfaceState::Live {
        return Decision::NoChange;
    }

    match policy {
        Policy::Never => Decision::NoChange,
        Policy::OnLastWindowClosed { .. } => {
            if input.open_windows == 0 {
                Decision::Suspend
            } else {
                Decision::NoChange
            }
        }
        Policy::OnHidden { .. } => {
            if input.visible_windows == 0 {
                Decision::Suspend
            } else {
                Decision::NoChange
            }
        }
        Policy::IdleTimeout { after_ms } => {
            if input.now.0.saturating_sub(input.last_interaction.0) >= *after_ms {
                Decision::Suspend
            } else {
                Decision::NoChange
            }
        }
    }
}
```

`saturating_sub` guards against a caller passing a `now` earlier than the reference instant,
which would otherwise panic in debug builds.

Update `crates/duet-core/src/lib.rs`:

```rust
pub mod lifecycle;
pub mod path;
pub mod policy;
pub mod store;
pub mod value;

pub use lifecycle::{Instant, InvalidTransition, LifecycleEvent, SurfaceState, transition};
pub use path::{Path, PathParseError, Segment};
pub use policy::{Decision, Policy, PolicyInput, evaluate};
pub use store::{Notification, Patch, Store, SubscriberId, SubscriptionId};
pub use value::{SetError, Value};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 60 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/src/policy.rs crates/duet-core/src/lib.rs
git commit -m "feat(core): add pure teardown policy evaluation"
```

---

## Task 11: End-to-end suspend/resume test

Proves the crate's headline claim: state survives a full teardown cycle.

**Files:**
- Create: `crates/duet-core/tests/suspend_resume.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/duet-core/tests/suspend_resume.rs`:

```rust
//! Integration test: state survives a full Live -> Cold -> Live cycle.

use duet_core::{
    Decision, Instant, LifecycleEvent, Path, Policy, PolicyInput, Store, SubscriberId,
    SurfaceState, Value, evaluate, transition,
};

fn p(s: &str) -> Path {
    Path::parse(s).expect("test path should parse")
}

#[test]
fn state_survives_teardown_and_is_restored_on_resume() {
    let mut store = Store::new(Value::map([(
        "editor",
        Value::map([("zoom", Value::Float(1.0))]),
    )]));

    let surface = SubscriberId(1);
    let policy = Policy::OnLastWindowClosed { grace_ms: 5_000 };

    // Surface comes up and subscribes.
    let mut state = transition(&SurfaceState::Cold, &LifecycleEvent::Start).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);
    store.subscribe(surface, p("editor.zoom"));

    // It writes something worth keeping.
    store.set(&p("editor.zoom"), Value::Float(3.5)).unwrap();

    // Last window closes.
    let input = PolicyInput {
        state: state.clone(),
        open_windows: 0,
        visible_windows: 0,
        last_interaction: Instant(0),
        now: Instant(1_000),
    };
    assert_eq!(evaluate(&policy, &input), Decision::Suspend);
    state = transition(&state, &LifecycleEvent::Suspend { at: Instant(1_000) }).unwrap();

    // Grace elapses, surface is torn down and its subscriptions dropped.
    let input = PolicyInput {
        state: state.clone(),
        now: Instant(6_000),
        ..input
    };
    assert_eq!(evaluate(&policy, &input), Decision::Teardown);
    state = transition(&state, &LifecycleEvent::GraceExpired).unwrap();
    assert_eq!(state, SurfaceState::Cold);
    assert_eq!(store.drop_subscriber(surface), 1);

    // Nothing reaches a cold surface.
    let notes = store.set(&p("editor.zoom"), Value::Float(4.0)).unwrap();
    assert!(notes.is_empty(), "a cold surface must receive nothing");

    // It comes back and re-subscribes.
    state = transition(&state, &LifecycleEvent::Resume).unwrap();
    state = transition(&state, &LifecycleEvent::Ready).unwrap();
    assert_eq!(state, SurfaceState::Live);

    let (_id, snapshot) = store.subscribe(surface, p("editor.zoom"));

    // The whole point: state written before teardown, plus the write that landed
    // while cold, are both present.
    assert_eq!(snapshot, Some(Value::Float(4.0)));
}

#[test]
fn resume_within_grace_returns_to_live_without_going_cold() {
    let state = SurfaceState::Suspending {
        since: Instant(1_000),
    };
    let resumed = transition(&state, &LifecycleEvent::Resume).unwrap();
    assert_eq!(resumed, SurfaceState::Live);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p duet-core --test suspend_resume`
Expected: FAIL — compilation error if any re-export is missing from `lib.rs`. If it compiles
and passes immediately, the earlier tasks were completed correctly; still run Step 4.

- [ ] **Step 3: Fix any missing re-exports**

If compilation failed, ensure `crates/duet-core/src/lib.rs` matches exactly:

```rust
pub mod lifecycle;
pub mod path;
pub mod policy;
pub mod store;
pub mod value;

pub use lifecycle::{Instant, InvalidTransition, LifecycleEvent, SurfaceState, transition};
pub use path::{Path, PathParseError, Segment};
pub use policy::{Decision, Policy, PolicyInput, evaluate};
pub use store::{Notification, Patch, Store, SubscriberId, SubscriptionId};
pub use value::{SetError, Value};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p duet-core`
Expected: PASS — 60 unit tests and 2 integration tests.

- [ ] **Step 5: Commit**

```bash
git add crates/duet-core/tests/suspend_resume.rs crates/duet-core/src/lib.rs
git commit -m "test(core): prove state survives a full teardown cycle"
```

---

## Task 12: Coverage gate

**Files:**
- Create: `.github/workflows/core.yml`

- [ ] **Step 1: Install the coverage tool**

Run: `cargo install cargo-llvm-cov`
Expected: `Installed package cargo-llvm-cov`. This takes a few minutes and is a one-time
cost. It is not currently installed on this machine.

- [ ] **Step 2: Measure coverage and confirm the gate passes**

Run: `cargo llvm-cov -p duet-core --fail-under-lines 90`
Expected: a per-file table, then exit code 0.

If it reports below 90%, read the table to find the uncovered lines and add tests for those
specific branches. Do **not** lower the threshold. Every uncovered line in this crate is a
behaviour nobody has checked, and this crate is the part of Duet with no excuse for that.

- [ ] **Step 3: Add CI**

Create `.github/workflows/core.yml`:

```yaml
name: duet-core

on:
  push:
    paths:
      - 'crates/duet-core/**'
      - 'Cargo.toml'
      - '.github/workflows/core.yml'
  pull_request:

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, llvm-tools-preview
      - uses: taiki-e/install-action@cargo-llvm-cov
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy -p duet-core --all-targets -- -D warnings
      - name: Test with coverage gate
        run: cargo llvm-cov -p duet-core --fail-under-lines 90
```

`duet-core` has no platform dependencies, so `ubuntu-latest` alone is sufficient here. The
per-OS matrix arrives in Phase 3 with the native crates.

- [ ] **Step 4: Verify formatting and lints locally**

Run: `cargo fmt --all && cargo clippy -p duet-core --all-targets -- -D warnings`
Expected: no output from clippy, exit code 0.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/core.yml crates/
git commit -m "ci: add duet-core test and coverage gate"
```

---

## Done criteria

- [ ] `cargo test -p duet-core` passes — 60 unit, 2 integration
- [ ] `cargo llvm-cov -p duet-core --fail-under-lines 90` exits 0
- [ ] `cargo clippy -p duet-core --all-targets -- -D warnings` is clean
- [ ] `crates/duet-core/Cargo.toml` still has an empty `[dependencies]`
- [ ] The integration test demonstrates state surviving `Live -> Cold -> Live`

## What Phase 1 deliberately does not build

Named so nobody adds them speculatively:

- **Proc macros / `#[derive(SharedState)]`** — Phase 4, and typed accessors layer on top of
  `Value` without changing it.
- **Serialization** — Phase 2 picks the codec. `Value` is codec-agnostic on purpose.
- **Threading** — spec §6.2's three-context model is Phase 2. `Store` is deliberately
  `Send`-friendly plain data with no interior mutability.
- **Commands and events router** — Phase 2, once there is a transport to route over.
- **Collection deltas** — spec §4.3 records `Vec` insert/remove as whole-vector replacement
  for v1. Do not optimise this here.
