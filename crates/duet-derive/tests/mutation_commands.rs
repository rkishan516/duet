//! Mutation testing for `#[command]`: break one thing at a time, and record
//! which check notices.
//!
//! # Why this file exists
//!
//! `src/command/generate_tests.rs` passes. So would a version of it that
//! compared the macro's output to a file the macro had just written, and so
//! would one whose assertion could not reach a failure at all. This project has
//! been bitten ten times by exactly that. The only way to know a check works is
//! to have watched it fail on purpose.
//!
//! Each mutation below is a **plausible developer edit** — a misspelled
//! argument key, a widened number — or a **plausible edit to the macro itself**,
//! spelled out by hand as the expansion a check-less macro would have produced.
//! The `every_check_passes_on_the_unmutated_command` test is what stops the
//! table being fiction: a check that fired on everything would score a perfect
//! catch rate while measuring nothing.
//!
//! # The two runtime checks
//!
//! | Check | What it compares | What it can see |
//! |---|---|---|
//! | `schema_bytes` | the rendered command against the correct one | everything the schema records, and so localises nothing |
//! | `live_reply` | the wire bytes for a canonical `invoke` | what a guest would actually receive |
//!
//! The interesting row is the camel collision. It changes **neither**: two keys
//! that camel-case alike are two perfectly good wire keys, and the collision
//! only exists in a generated Dart or TypeScript client. Nothing behavioural can
//! see it, which is why the check is at compile time and why removing it would
//! leave every runtime test in this workspace green.

use duet::{
    Args, Command, CommandContext, CommandDef, CommandEntry, CommandParam, Commands, FieldDef,
    Outcome, Registry, Schema, SubscriberId, Value, command, command_raises, command_returns,
    commands, describe, into_outcome,
};
use duet_protocol::handle_text_with;
use duet_runtime::{NullSink, Runtime};

/// The command the mutants are measured against: `a - b`, both `i64`.
#[command]
fn subtract(a: i64, b: i64) -> i64 {
    a.saturating_sub(b)
}

/// A key with a typo in it. The kind of edit a `rename` invites.
mod key_spelling {
    use super::command;

    #[command(rename = "subtract")]
    pub fn subtract(a: i64, #[duet(rename = "c")] b: i64) -> i64 {
        a.saturating_sub(b)
    }
}

/// An argument widened from `i64` to `f64`. Compiles, runs, and means something
/// different on the wire.
mod type_mapping {
    use super::command;

    #[command(rename = "subtract")]
    #[allow(clippy::cast_precision_loss)]
    pub fn subtract(a: f64, b: i64) -> i64 {
        (a - b as f64) as i64
    }
}

/// The return type widened. The schema's spelling of it changes, and so does
/// every guest decoder built against it.
mod return_mapping {
    use super::command;

    #[command(rename = "subtract")]
    #[allow(clippy::cast_precision_loss)]
    pub fn subtract(a: i64, b: i64) -> f64 {
        (a - b) as f64
    }
}

/// The expansion a macro **without the camel-collision check** would produce.
///
/// Hand-written rather than generated, because the mutation is to the macro and
/// not to its input: `#[command] fn f(font_size: i64, fontSize: i64)` does not
/// compile today, which is the whole point. This is what it would compile to if
/// it did, so the runtime checks have something to fail to notice.
mod camel_collision {
    use super::{
        Args, Command, CommandContext, CommandDef, CommandParam, FieldDef, Outcome, Registry,
        command_raises, command_returns, into_outcome,
    };

    #[allow(non_camel_case_types)]
    pub struct subtract {}

    impl Command for subtract {
        const NAME: &'static str = "subtract";

        fn describe(registry: &mut Registry) -> CommandDef {
            CommandDef {
                name: "subtract".to_string(),
                params: Vec::from([
                    FieldDef::new("a", <i64 as CommandParam>::param_ty(registry)),
                    // `b` and `B` are two distinct wire keys that both become
                    // the accessor `b`.
                    FieldDef::new("b", <i64 as CommandParam>::param_ty(registry)),
                    FieldDef::new("B", <i64 as CommandParam>::param_ty(registry)),
                ]),
                returns: command_returns::<i64, _>(registry),
                raises: command_raises::<i64, _>(registry),
            }
        }

        fn run(args: Args, _context: &CommandContext) -> Outcome {
            let a = match <i64 as CommandParam>::from_args("a", &args) {
                Ok(value) => value,
                Err(why) => return Outcome::Refused(why),
            };
            let b = match <i64 as CommandParam>::from_args("b", &args) {
                Ok(value) => value,
                Err(why) => return Outcome::Refused(why),
            };
            into_outcome(a.saturating_sub(b))
        }
    }
}

/// One check, and whether it noticed.
struct Report {
    name: &'static str,
    caught: bool,
}

/// The canonical invocation every mutant is driven with.
const REQUEST: &str = r#"{"kind":"invoke","id":"1","command":"subtract","args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}}}"#;

/// What the correct command answers, and how the correct schema renders it.
const CORRECT_REPLY: &str = r#"{"id":"1","kind":"returned","value":{"t":"i","v":"7"}}"#;

/// The rendered schema of one command.
fn rendered(entries: &'static [CommandEntry]) -> String {
    Schema::of_with_commands::<i64>(|registry| describe(entries, registry)).map_or_else(
        |errors| format!("refused: {errors}"),
        |schema| schema.render(),
    )
}

/// The reply one command gives to [`REQUEST`].
fn replied(entries: &'static [CommandEntry]) -> String {
    let runtime = Runtime::spawn(Value::Null, NullSink);
    let reply = handle_text_with(
        &runtime.handle(),
        SubscriberId(1),
        &Commands::from_entries(entries),
        REQUEST,
    );
    runtime.shutdown().expect("shutdown should succeed");
    reply
}

/// Runs both checks over one command table.
fn checks(entries: &'static [CommandEntry]) -> Vec<Report> {
    vec![
        Report {
            name: "schema_bytes",
            caught: rendered(entries) != rendered(&CORRECT),
        },
        Report {
            name: "live_reply",
            caught: replied(entries) != CORRECT_REPLY,
        },
    ]
}

/// The names of the checks that noticed.
fn caught_by(entries: &'static [CommandEntry]) -> Vec<&'static str> {
    checks(entries)
        .into_iter()
        .filter(|report| report.caught)
        .map(|report| report.name)
        .collect()
}

static CORRECT: [CommandEntry; 1] = commands![subtract];
static KEY_SPELLING: [CommandEntry; 1] = commands![key_spelling::subtract];
static TYPE_MAPPING: [CommandEntry; 1] = commands![type_mapping::subtract];
static RETURN_MAPPING: [CommandEntry; 1] = commands![return_mapping::subtract];
static CAMEL_COLLISION: [CommandEntry; 1] = commands![camel_collision::subtract];

#[test]
fn every_check_passes_on_the_unmutated_command() {
    // The measurement that makes the table below mean anything. Without it a
    // check that always fired would look like the most sensitive one here.
    assert_eq!(caught_by(&CORRECT), Vec::<&str>::new());
    assert_eq!(replied(&CORRECT), CORRECT_REPLY);
}

#[test]
fn a_misspelled_argument_key_is_caught_by_both() {
    // The schema records `c` where `b` belongs, and the live call refuses
    // because nothing supplies `c`.
    assert_eq!(caught_by(&KEY_SPELLING), ["schema_bytes", "live_reply"]);
    assert!(replied(&KEY_SPELLING).contains("failed"));
}

#[test]
fn a_widened_argument_is_caught_by_both() {
    assert_eq!(caught_by(&TYPE_MAPPING), ["schema_bytes", "live_reply"]);
}

#[test]
fn a_widened_return_type_is_caught_by_both() {
    // The schema's spelling of the return changes from `int` to `float`, and
    // the wire tag changes with it — which is the point of keeping `describe`
    // and `run` on one impl.
    assert_eq!(caught_by(&RETURN_MAPPING), ["schema_bytes", "live_reply"]);
    assert!(rendered(&RETURN_MAPPING).contains("\"returns\": {\"kind\": \"float\"}"));
    assert!(replied(&RETURN_MAPPING).contains("\"t\":\"f\""));
}

#[test]
fn dropping_the_camel_collision_check_is_caught_by_nothing_behavioural() {
    // THE row of the table. Two keys that camel-case alike are two perfectly
    // good wire keys: the schema is valid, the call succeeds, and the reply is
    // byte-identical to the correct one. The collision exists only in a
    // generated Dart or TypeScript client, which is why the check has to be at
    // compile time.
    assert_eq!(caught_by(&CAMEL_COLLISION), ["schema_bytes"]);
    assert_eq!(
        replied(&CAMEL_COLLISION),
        CORRECT_REPLY,
        "the mutant answers correctly, which is exactly the problem"
    );
    // And `schema_bytes` catches it only because this mutant also has a third
    // argument — it notices *a* difference, not *the* difference. Two keys
    // colliding with no other change would be invisible to both.
    assert!(rendered(&CAMEL_COLLISION).contains("\"key\": \"B\""));
}

#[test]
fn the_camel_collision_check_is_caught_by_exactly_two_compile_time_checks() {
    // Named rather than run, because a check that refuses at compile time
    // cannot be exercised from a test that has to compile. Both are asserted
    // elsewhere and both are listed here so that deleting either is visible as
    // a broken cross-reference rather than as a silent loss of coverage:
    //
    //   - `command::model::tests::two_parameters_that_camel_case_alike_are_refused`
    //     in `src/command/model_tests.rs`
    //   - `tests/ui/command_camel_collision.rs`, whose committed `.stderr`
    //     `tests/compile_fail.rs` asserts still names the fix
    //
    // The file check is real: a UI case someone deleted would fail here.
    let case = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("ui")
        .join("command_camel_collision.rs");
    assert!(
        case.is_file(),
        "the only behavioural-invisible check lost its compile-fail case: {}",
        case.display()
    );
    assert!(case.with_extension("stderr").is_file());
}
