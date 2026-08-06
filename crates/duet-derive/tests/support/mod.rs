//! Reading the committed schema specifications, and reporting a difference.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

/// The repository root, found from this crate rather than from the working
/// directory, which `cargo test` does not promise.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Reads a file under the repository root.
pub fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Asserts that a derived schema is **byte-identical** to a committed one.
///
/// # Why this is the load-bearing check of the whole increment
///
/// `schema/app.json` was hand-written, and every emitter, golden file and
/// conformance suite downstream was built against it before this macro existed.
/// It is therefore an **independent target**: the derive has to hit a
/// specification it did not get a vote on. A test that regenerated the file
/// through the derive and then compared the derive against it would prove
/// nothing at all — it would pass for any macro, including one that spelled
/// every key backwards.
///
/// So the comparison is against the committed bytes, read from disk at test
/// time, and it is a byte comparison rather than a `Schema` equality: two
/// `Schema` values can be equal while rendering differently, and it is the
/// rendered file that `duet-codegen`, `duet-host-stdio` and both guest packages
/// actually read.
pub fn assert_matches_committed(relative: &str, derived: &str) {
    let committed = read(relative);
    assert!(
        committed.as_bytes() == derived.as_bytes(),
        "the derived schema is not {relative}{}\n\n\
         `{relative}` is the hand-written specification the derive has to satisfy. If the derive \
         is wrong, fix the derive. If the specification is wrong, that is a finding: change it in \
         a separate commit, before this one, with the diff explained — a macro that edits the \
         specification it is checked against is checking nothing.",
        difference(&committed, derived)
    );
}

/// Names the first differing line, so a failure points at the change rather
/// than dumping two copies of a file.
fn difference(committed: &str, derived: &str) -> String {
    for (n, (a, b)) in committed.lines().zip(derived.lines()).enumerate() {
        if a != b {
            return format!(
                " — first difference at line {}:\n  committed: {a}\n    derived: {b}",
                n + 1
            );
        }
    }
    format!(
        " — identical for {} lines, then the length differs ({} committed vs {} derived)",
        committed.lines().count().min(derived.lines().count()),
        committed.lines().count(),
        derived.lines().count()
    )
}
