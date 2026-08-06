//! This host and the code generator must be seeded from the same schemas.
//!
//! `corpus/schema-corpus.json` states, per schema, the seed this host starts
//! from and every path the generated clients bind. A schema that
//! `crates/duet-codegen` generates goldens for but this host cannot be seeded
//! from would leave a corpus entry no live-host run can exercise — and the run
//! would still pass, having quietly skipped it. A schema this host knows and
//! the generator does not is the mirror image: a fixture nothing describes.
//!
//! Neither list can be derived from the other without one of them ceasing to be
//! an independent statement, so they are compared instead.

/// The schema files `crates/duet-codegen`'s goldens are generated from.
///
/// Transcribed rather than imported: `duet-codegen`'s `FIXTURES` lives in its
/// `tests/support` module, which is not part of its public API and cannot be
/// reached from another crate. A transcription that drifts is exactly what the
/// assertion below is for.
const CODEGEN_FIXTURES: &[&str] = &["schema/app.json", "schema/wide.json"];

#[test]
fn this_host_can_be_seeded_from_every_schema_the_generator_uses() {
    let mine: Vec<&str> = duet_host_stdio::FIXTURES.iter().map(|f| f.source).collect();
    assert_eq!(
        mine, CODEGEN_FIXTURES,
        "the schemas this host embeds and the schemas the goldens are generated \
         from have diverged; add the missing one to `duet_host_stdio::FIXTURES` \
         (src/fixture.rs) or to `crates/duet-codegen/tests/support/mod.rs`"
    );
}

#[test]
fn the_transcribed_list_is_still_what_the_generator_declares() {
    // The other half: if `duet-codegen`'s own list changed, the transcription
    // above is now the stale one and the assertion it makes is worthless.
    let declared = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../duet-codegen/tests/support/mod.rs"),
    )
    .expect("duet-codegen's fixture list should be readable");
    for source in CODEGEN_FIXTURES {
        assert!(
            declared.contains(&format!("schema: \"{source}\"")),
            "{source} is no longer declared in duet-codegen's FIXTURES"
        );
    }
    assert_eq!(
        declared.matches("schema: \"schema/").count(),
        CODEGEN_FIXTURES.len(),
        "duet-codegen declares a different number of fixtures than this file \
         transcribes; update CODEGEN_FIXTURES in tests/fixtures.rs"
    );
}
