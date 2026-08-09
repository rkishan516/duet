//! The Duet showcase, minus the platform.
//!
//! This library is the half of the showcase that is just *definition*: the
//! shared state ([`state`]), the host commands ([`commands`]), and the schema
//! they render to ([`schema`]). It builds on every platform, has no window and
//! no guest, and is what `src/bin/schema.rs` runs to write
//! `examples/showcase/schema/showcase.json` out.
//!
//! The other half — two real guests, a real event loop and the tour that drives
//! them — is macOS-only and lives in the binary, `src/main.rs`.
//!
//! ```text
//!   state.rs + commands.rs          the one definition
//!            │
//!            ├─ Schema::of_with_commands  ──▶  schema/showcase.json
//!            │                                        │
//!            │                                  duet generate
//!            │                                   │          │
//!            │                       flutter/lib/src/    web/src/
//!            │                       showcase.duet.dart  showcase.duet.ts
//!            │
//!            └─ install(handle, &initial_state())  ──▶  the live store
//! ```

#![deny(missing_docs)]

pub mod commands;
pub mod state;

use duet::{Schema, SchemaErrors, describe};

use crate::commands::COMMANDS;
use crate::state::Showcase;

/// The showcase's schema document: the state *and* the commands.
///
/// [`Schema::of`] would describe the state alone, and the resulting document
/// would not mention `append_line`, `word_count`, or
/// [`ComposeError`](crate::state::ComposeError) — which is reachable only as a
/// command's `raises`. Generated clients would then compile and simply have no
/// command methods, which is the kind of gap that is invisible until a guest
/// needs one.
///
/// # Errors
///
/// Returns [`SchemaErrors`] if the definition cannot be described — a name
/// collision, a cycle, or a type with no faithful spelling on the wire. In this
/// crate that is a compile-time-adjacent failure: the shapes are fixed, so it
/// can only start failing when someone edits [`state`] or [`commands`].
pub fn schema() -> Result<Schema, SchemaErrors> {
    Schema::of_with_commands::<Showcase>(|registry| describe(&COMMANDS, registry))
}

#[cfg(test)]
mod tests {
    use duet::runtime::NullSink;
    use duet::{Reading, Runtime, Value, install};

    use super::*;
    use crate::state::{ComposeError, Document, HostNote, Presence, initial_state};

    /// The committed schema document, as it is on disk.
    const COMMITTED: &str = include_str!("../../schema/showcase.json");

    /// The definition and the committed contract are the same document.
    ///
    /// Without this, the two guests' generated clients could be regenerated from
    /// a schema the Rust host no longer produces and nothing would notice: every
    /// language still compiles, and the disagreement shows up as a field that is
    /// silently always absent. Byte-for-byte, because
    /// [`Schema::render`](duet::Schema::render) is deterministic and a
    /// "semantically equal" comparison would let formatting drift make the
    /// committed file stop being what `duet generate` was actually run against.
    #[test]
    fn the_committed_schema_is_what_the_definition_renders() {
        let rendered = schema().expect("the showcase definition should describe cleanly");
        assert_eq!(
            rendered.render(),
            COMMITTED,
            "examples/showcase/schema/showcase.json is stale; regenerate it with \
             `cargo run -p duet-showcase --bin schema > examples/showcase/schema/showcase.json` \
             and then re-run `duet generate`"
        );
    }

    /// `install` writes the whole seeded tree, and typed fields read it back.
    #[test]
    fn installing_the_initial_state_makes_every_field_readable() {
        let runtime = Runtime::spawn(Value::Null, NullSink);
        let store = install(runtime.handle(), &initial_state())
            .expect("the initial state should install into an empty store");

        let title = store
            .field::<String>("document.title")
            .expect("document.title is a valid path");
        assert_eq!(title.get(), Ok(Reading::Present("untitled".to_string())));

        let document = store
            .field::<Document>("document")
            .expect("document is a valid path");
        assert_eq!(
            document.get(),
            Ok(Reading::Present(Document {
                title: "untitled".to_string(),
                lines: Vec::new(),
            }))
        );

        let status = store
            .field::<String>("flutter.status")
            .expect("flutter.status is a valid path");
        assert_eq!(status.get(), Ok(Reading::Present("booting".to_string())));

        // Both guests' subtrees, read whole. The tour prints these at the end,
        // and a `Presence` that would not decode back out of the store is a
        // report that quietly says `Mismatch` instead of showing the evidence.
        let flutter = store
            .field::<Presence>("flutter")
            .expect("flutter is a valid path");
        assert_eq!(flutter.get(), Ok(Reading::Present(Presence::booting())));
        let host = store.field::<HostNote>("host").expect("host is a path");
        assert_eq!(host.get(), Ok(Reading::Present(initial_state().host)));

        runtime.shutdown().expect("the core thread should stop");
    }

    /// `append_line` appends, counts, and raises rather than panicking.
    ///
    /// Called as a plain Rust function: `#[command]` re-emits the body
    /// unchanged, so the thing a guest invokes over the wire and the thing this
    /// test calls are the same function.
    #[test]
    fn append_line_appends_and_refuses_a_blank_line() {
        use duet::CommandContext;

        let runtime = Runtime::spawn(Value::Null, NullSink);
        install(runtime.handle(), &initial_state()).expect("the initial state should install");
        let ctx = CommandContext::new(runtime.handle());

        assert_eq!(commands::append_line(&ctx, "first".to_string()), Ok(1));
        assert_eq!(commands::append_line(&ctx, "second".to_string()), Ok(2));
        assert_eq!(
            commands::append_line(&ctx, "   ".to_string()),
            Err(ComposeError::new(
                "empty_line",
                "a line needs at least one non-blank character"
            ))
        );

        let lines = runtime
            .handle()
            .get(&duet::Path::parse(commands::LINES_PATH).expect("a valid path"))
            .expect("the store should answer");
        assert_eq!(
            lines,
            Some(Value::List(vec![
                Value::Str("first".to_string()),
                Value::Str("second".to_string()),
            ]))
        );

        runtime.shutdown().expect("the core thread should stop");
    }

    /// `word_count` is total, including on the empty string.
    #[test]
    fn word_count_counts_whitespace_separated_words() {
        assert_eq!(commands::word_count(String::new()), 0);
        assert_eq!(commands::word_count("  ".to_string()), 0);
        assert_eq!(commands::word_count("one two  three\nfour".to_string()), 4);
    }

    /// The `raises` type survives the wire in both directions.
    ///
    /// `ComposeError` is reachable from the schema only through a command, so
    /// nothing in the state ever encodes or decodes one. The generated Dart and
    /// TypeScript clients do, on every raise — this is the Rust half of the same
    /// round trip.
    #[test]
    fn the_raises_type_round_trips_through_a_value() {
        use duet::SharedState;

        let error = ComposeError::new("empty_line", "a line needs a character");
        assert_eq!(ComposeError::from_value(&error.to_value()), Ok(error));
    }
}
