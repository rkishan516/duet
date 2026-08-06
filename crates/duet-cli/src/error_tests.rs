//! Exit codes and messages.

use super::*;

#[test]
fn the_four_exit_codes_are_distinct() {
    // A caller that cannot tell a stale file from a broken schema has to parse
    // prose, which is how a gate silently stops gating.
    let codes = [0, EXIT_FAILED, EXIT_USAGE, EXIT_STALE];
    let unique: std::collections::BTreeSet<u8> = codes.into_iter().collect();
    assert_eq!(unique.len(), codes.len(), "{codes:?}");
}

#[test]
fn each_error_maps_to_the_code_a_script_expects() {
    assert_eq!(
        CliError::Usage(UsageError::NoSchema).exit_code(),
        EXIT_USAGE
    );
    assert_eq!(CliError::Stale(Vec::new()).exit_code(), EXIT_STALE);
    assert_eq!(
        CliError::ReadSchema {
            path: PathBuf::from("s.json"),
            source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
        }
        .exit_code(),
        EXIT_FAILED
    );
    assert_eq!(
        CliError::Emit(EmitError::TooManyClasses { max: 256 }).exit_code(),
        EXIT_FAILED
    );
}

#[test]
fn every_message_names_the_path_it_is_about() {
    let read = CliError::ReadSchema {
        path: PathBuf::from("schema/app.json"),
        source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
    };
    assert!(read.to_string().contains("schema/app.json"), "{read}");
    assert!(read.to_string().contains("no such file"), "{read}");

    let output = CliError::ReadOutput {
        path: PathBuf::from("lib/a.dart"),
        source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
    };
    assert!(output.to_string().contains("lib/a.dart"), "{output}");
}

#[test]
fn a_schema_rejection_reaches_the_terminal_unaltered() {
    // The CLI must not paraphrase `duet-codegen`; that crate's messages are
    // the ones with the field names in them.
    let source = duet_codegen::read_schema("{").expect_err("not JSON");
    let inner = source.to_string();
    let error = CliError::Schema {
        path: PathBuf::from("schema/app.json"),
        source,
    };
    let message = error.to_string();
    assert!(message.contains(&inner), "{message} lost {inner}");
    assert!(message.contains("schema/app.json"), "{message}");
}

#[test]
fn an_emit_rejection_reaches_the_terminal_unaltered() {
    let inner = EmitError::AccessorCollision {
        type_name: "App".to_string(),
        accessor: "fooBar".to_string(),
    };
    let expected = inner.to_string();
    assert_eq!(CliError::Emit(inner).to_string(), expected);
}

#[test]
fn a_stale_summary_counts_the_files() {
    let error = CliError::Stale(vec![
        Difference {
            path: PathBuf::from("a.dart"),
            missing: false,
            diff: "x".to_string(),
        },
        Difference {
            path: PathBuf::from("b.ts"),
            missing: true,
            diff: String::new(),
        },
    ]);
    assert!(error.to_string().contains('2'), "{error}");
}

#[test]
fn every_variant_reports_a_source_or_deliberately_does_not() {
    let with_source: Vec<CliError> = vec![
        CliError::Usage(UsageError::NoSchema),
        CliError::ReadSchema {
            path: PathBuf::new(),
            source: io::Error::other("x"),
        },
        CliError::ReadOutput {
            path: PathBuf::new(),
            source: io::Error::other("x"),
        },
        CliError::Schema {
            path: PathBuf::new(),
            source: duet_codegen::read_schema("{").expect_err("not JSON"),
        },
        CliError::Emit(EmitError::TooManyClasses { max: 1 }),
        CliError::Write(WriteError {
            path: PathBuf::new(),
            doing: "write",
            source: io::Error::other("x"),
        }),
    ];
    for error in &with_source {
        assert!(
            std::error::Error::source(error).is_some(),
            "{error:?} hides its cause"
        );
        assert!(!error.to_string().is_empty(), "{error:?}");
    }
    // A staleness report is not caused by anything; it *is* the finding.
    assert!(std::error::Error::source(&CliError::Stale(Vec::new())).is_none());
}

#[test]
fn the_conversions_exist_so_the_run_can_use_the_question_mark() {
    let from_usage: CliError = UsageError::NoOutput.into();
    assert_eq!(from_usage.exit_code(), EXIT_USAGE);
    let from_emit: CliError = EmitError::TooManyClasses { max: 2 }.into();
    assert_eq!(from_emit.exit_code(), EXIT_FAILED);
    let from_write: CliError = WriteError {
        path: PathBuf::new(),
        doing: "write",
        source: io::Error::other("x"),
    }
    .into();
    assert_eq!(from_write.exit_code(), EXIT_FAILED);
}
