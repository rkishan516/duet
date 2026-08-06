//! The schema fixtures this host can be seeded from.
//!
//! Compiled in with [`include_str!`] rather than read from disk. Three reasons,
//! all of them about a harness being trustworthy:
//!
//! - **No path resolution at runtime.** A binary that looked for
//!   `schema/app.json` relative to its working directory would answer
//!   differently depending on where a test runner happened to `cd`, and the
//!   failure mode is a host seeded with the wrong shape rather than an error.
//! - **A schema edit rebuilds the binary.** `include_str!` is a build
//!   dependency, so a stale host is not a state this can be in.
//! - **No file I/O on the startup path**, so the only way `Session::open` can
//!   fail is a name it does not know.

use duet_schema::Schema;

use crate::error::SessionError;

/// One embedded schema fixture.
#[derive(Debug, Clone, Copy)]
pub struct Fixture {
    /// The name a caller asks for.
    pub name: &'static str,
    /// Where it lives in the repository, for a diagnostic.
    pub source: &'static str,
    /// The rendered schema itself.
    pub text: &'static str,
}

/// Every fixture, in the order [`names`] reports them.
///
/// The same two schemas `crates/duet-codegen`'s goldens are generated from, and
/// the same two `corpus/schema-corpus.json` describes. That is not a
/// coincidence to be maintained by hand: `tests/fixtures.rs` asserts this list
/// and `duet-codegen`'s `FIXTURES` name the same files, so a schema added to
/// one and not the other fails rather than leaving a host that cannot be seeded
/// for a corpus entry that exists.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "app",
        source: "schema/app.json",
        text: include_str!("../../../schema/app.json"),
    },
    Fixture {
        name: "wide",
        source: "schema/wide.json",
        text: include_str!("../../../schema/wide.json"),
    },
];

/// Every fixture name, for a usage message.
#[must_use]
pub fn names() -> Vec<&'static str> {
    FIXTURES.iter().map(|f| f.name).collect()
}

/// How much of a rejected name is echoed back.
///
/// argv is not attacker-controlled in the way guest text is, but the rule that
/// a host never echoes unbounded input back is worth keeping uniform: it is the
/// rule that stops one careless call site turning a megabyte of garbage into a
/// megabyte of host-produced text.
const MAX_ECHOED_NAME_CHARS: usize = 64;

/// Looks up a fixture by name and parses its schema.
///
/// # Errors
///
/// [`SessionError::UnknownFixture`] if no fixture goes by `name`, and
/// [`SessionError::UnreadableFixture`] if the embedded text no longer parses.
pub fn schema(name: &str) -> Result<(Fixture, Schema), SessionError> {
    let Some(fixture) = FIXTURES.iter().find(|f| f.name == name) else {
        return Err(SessionError::UnknownFixture {
            asked: name.chars().take(MAX_ECHOED_NAME_CHARS).collect(),
            known: names(),
        });
    };
    let schema =
        duet_codegen::read_schema(fixture.text).map_err(|e| SessionError::UnreadableFixture {
            name: fixture.name,
            because: e.to_string(),
        })?;
    Ok((*fixture, schema))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_fixture_parses_and_seeds() {
        // The check that makes `include_str!` safe: a schema edited into a
        // shape the reader refuses fails here, at `cargo test`, rather than in
        // a host process a guest is already waiting on.
        for fixture in FIXTURES {
            let (found, schema) = schema(fixture.name)
                .unwrap_or_else(|e| panic!("{} should parse: {e}", fixture.source));
            assert_eq!(found.name, fixture.name);
            let seed = duet_schema::seed(&schema);
            assert!(
                matches!(seed, duet_core::Value::Map(_)),
                "{} should seed a map at the root, got {seed:?}",
                fixture.source
            );
        }
    }

    #[test]
    fn the_embedded_text_is_the_committed_file() {
        // `include_str!` bakes in whatever path it was given, and a wrong one
        // would compile happily against the wrong schema. Comparing against the
        // file read at test time is what says the path is right.
        for fixture in FIXTURES {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join(fixture.source);
            let on_disk = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            assert_eq!(
                fixture.text, on_disk,
                "the text compiled into this binary is not {}",
                fixture.source
            );
        }
    }

    #[test]
    fn an_unknown_name_lists_the_known_ones_and_bounds_what_it_echoes() {
        let error = schema("nope").expect_err("an unknown fixture must be refused");
        assert_eq!(
            error,
            SessionError::UnknownFixture {
                asked: "nope".to_string(),
                known: vec!["app", "wide"],
            }
        );

        let huge = "z".repeat(1_000_000);
        let error = schema(&huge).expect_err("an unknown fixture must be refused");
        assert!(
            error.to_string().len() < 512,
            "a refusal must not echo argv whole, got {} bytes",
            error.to_string().len()
        );
    }

    #[test]
    fn the_names_are_unique_and_non_empty() {
        // Two fixtures on one name would make `schema` answer with whichever
        // came first, silently.
        let mut seen: Vec<&str> = Vec::new();
        for name in names() {
            assert!(!name.is_empty());
            assert!(!seen.contains(&name), "duplicate fixture name {name}");
            seen.push(name);
        }
        assert!(!seen.is_empty(), "a host with no fixtures cannot be seeded");
    }
}
