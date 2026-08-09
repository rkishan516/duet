//! Writes the showcase's schema document to stdout.
//!
//! ```console
//! $ cargo run -p duet-showcase --bin schema > examples/showcase/schema/showcase.json
//! ```
//!
//! The committed file is the contract: `duet generate` reads it, the two guests'
//! clients are emitted from it, and `duet-showcase`'s own
//! `the_committed_schema_is_what_the_definition_renders` test fails if it stops
//! matching the Rust definition. Nothing reads this binary at run time — it
//! exists so the contract is *produced* rather than maintained by hand.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `print!`, not `println!`: `Schema::render` already ends in exactly one
    // newline, and a second one would make the committed file differ from what
    // the staleness test compares against.
    print!("{}", duet_showcase::schema()?.render());
    Ok(())
}
