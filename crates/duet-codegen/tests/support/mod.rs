//! Where the hand-written schemas and the committed goldens live, and how one
//! becomes the other.
//!
//! Shared by every integration test in this crate so that the staleness check,
//! the determinism check and the regeneration all run the *same* generation.
//! Two descriptions of "what the goldens should contain" would eventually
//! disagree, and the one that disagreed silently would be the check.

// Every integration test binary compiles this file whole and uses a different
// part of it. Without this, adding a helper for one test warns in every other.
#![allow(dead_code)]

pub mod checks;

use std::fs;
use std::path::{Path, PathBuf};

use duet_codegen::{Generated, Options, TsImports};
use duet_schema::Schema;

/// The exact command that rewrites every committed golden.
///
/// Named in each generated file's header and in the staleness failure. A
/// generated file that says "do not edit" without saying what to run instead is
/// a file people edit.
pub const GENERATOR: &str =
    "cargo test -p duet-codegen --test goldens -- --ignored regenerate_goldens";

/// One hand-written schema and the two files generated from it.
pub struct Fixture {
    /// The basename shared by the generated files.
    pub stem: &'static str,
    /// The schema, relative to the repository root.
    pub schema: &'static str,
}

/// Every schema the goldens are generated from.
///
/// `app` is the canonical example increment 6's derive must reproduce
/// byte-for-byte. `wide` exists to reach every arm of the schema's type
/// language; `tests/coverage.rs` asserts mechanically that between them they do.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        stem: "app",
        schema: "schema/app.json",
    },
    Fixture {
        stem: "wide",
        schema: "schema/wide.json",
    },
];

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

/// The schema named by `relative`, parsed and validated.
pub fn schema(relative: &str) -> Schema {
    duet_codegen::read_schema(&read(relative))
        .unwrap_or_else(|e| panic!("{relative} should be a valid schema: {e}"))
}

impl Fixture {
    /// Where this fixture's Dart golden lives, relative to the repository root.
    pub fn dart_path(&self) -> String {
        format!("packages/duet/test/generated/{}.duet.dart", self.stem)
    }

    /// Where this fixture's TypeScript golden lives.
    pub fn ts_path(&self) -> String {
        format!("packages/duet-js/test/generated/{}.duet.ts", self.stem)
    }

    /// How this fixture is generated.
    ///
    /// The TypeScript imports reach into `src` rather than naming the published
    /// package, because the goldens live *inside* `packages/duet-js`, where the
    /// package cannot import itself by name without a build having run first —
    /// which would make `npm run typecheck` depend on `npm run build`. Every
    /// other test file in that package reaches into `src` the same way.
    pub fn options(&self) -> Options {
        Options {
            source: self.schema.to_string(),
            command: GENERATOR.to_string(),
            ts_imports: TsImports {
                root: "../../src/index.ts".to_string(),
                typed: "../../src/typed/index.ts".to_string(),
            },
        }
    }

    /// Generates this fixture's two files.
    pub fn generate(&self) -> Generated {
        duet_codegen::generate(&schema(self.schema), &self.options())
            .unwrap_or_else(|e| panic!("{} should be emittable: {e}", self.schema))
    }

    /// The two (path, content) pairs this fixture is responsible for.
    pub fn outputs(&self) -> Vec<(String, String)> {
        let generated = self.generate();
        vec![
            (self.dart_path(), generated.dart),
            (self.ts_path(), generated.ts),
        ]
    }
}

/// Every committed golden, regenerated in memory.
pub fn all_outputs() -> Vec<(String, String)> {
    FIXTURES.iter().flat_map(Fixture::outputs).collect()
}

/// Writes `content` to `relative`, creating its directory.
pub fn write(relative: &str, content: &str) {
    let path = repo_root().join(relative);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
    }
    fs::write(&path, content).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}

/// Names the first differing line, so a failure points at the change rather
/// than dumping two copies of a file.
pub fn first_difference(committed: &str, generated: &str) -> String {
    for (n, (a, b)) in committed.lines().zip(generated.lines()).enumerate() {
        if a != b {
            return format!(
                " — first difference at line {}:\n  committed: {a}\n  generated: {b}",
                n + 1
            );
        }
    }
    format!(
        " — identical for {} lines, then the length differs ({} committed vs {} generated)",
        committed.lines().count().min(generated.lines().count()),
        committed.lines().count(),
        generated.lines().count()
    )
}

/// Every schema file under `schema/<dir>`, sorted by name.
///
/// Sorted so the order a test reports failures in is a function of the
/// directory rather than of the filesystem.
pub fn schema_files(dir: &str) -> Vec<PathBuf> {
    let root = repo_root().join("schema").join(dir);
    let mut found: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", root.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "{} holds no schemas", root.display());
    found
}

/// A file's basename without its extension, for a failure message.
pub fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
