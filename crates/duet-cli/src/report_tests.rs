//! What a failure looks like on stderr.

use super::*;
use crate::args::UsageError;
use std::path::PathBuf;

/// A difference at `path` whose diff is `diff`.
fn differs(path: &str, diff: &str) -> Difference {
    Difference {
        path: PathBuf::from(path),
        missing: false,
        diff: diff.to_string(),
    }
}

#[test]
fn a_usage_failure_points_at_the_help() {
    let report = failure(&CliError::Usage(UsageError::NoSchema), None);
    assert!(report.starts_with("duet: "), "{report}");
    assert!(report.contains("--schema is required"), "{report}");
    assert!(report.contains("duet --help"), "{report}");
    assert!(report.ends_with('\n'));
}

#[test]
fn an_ordinary_failure_is_one_line() {
    let report = failure(
        &CliError::Emit(duet_codegen::EmitError::TooManyClasses { max: 256 }),
        Some("duet generate --schema s.json --ts a.ts"),
    );
    assert_eq!(report.lines().count(), 1, "{report}");
    assert!(report.starts_with("duet: "), "{report}");
}

#[test]
fn a_stale_report_names_the_file_the_line_and_the_command() {
    // All three, because each one alone leaves the reader guessing: the file
    // without the line means opening it, the line without the command means
    // hunting for the generator, and the command without either means running
    // it blind.
    let command = "duet generate --schema schema/app.json --dart lib/a.dart";
    let report = failure(
        &CliError::Stale(vec![differs(
            "lib/a.dart",
            "-     4 | // Regenerate: old\n+     4 | // Regenerate: new\n",
        )]),
        Some(command),
    );
    assert!(
        report.contains("1 generated file(s) are out of date"),
        "{report}"
    );
    assert!(report.contains("lib/a.dart:"), "{report}");
    assert!(report.contains("// Regenerate: new"), "{report}");
    assert!(report.contains("Regenerate with:"), "{report}");
    assert!(report.contains(command), "{report}");
}

#[test]
fn every_stale_file_is_named_rather_than_only_the_first() {
    // A schema change moves both clients; reporting one per CI run wastes an
    // afternoon.
    let report = failure(
        &CliError::Stale(vec![differs("a.dart", "x\n"), differs("b.ts", "y\n")]),
        Some("duet generate --schema s.json --dart a.dart --ts b.ts"),
    );
    assert!(report.contains("2 generated file(s)"), "{report}");
    assert!(report.contains("a.dart:"), "{report}");
    assert!(report.contains("b.ts:"), "{report}");
}

#[test]
fn a_file_that_does_not_exist_says_so_rather_than_showing_an_empty_diff() {
    let report = failure(
        &CliError::Stale(vec![Difference {
            path: PathBuf::from("lib/a.dart"),
            missing: true,
            diff: String::new(),
        }]),
        Some("duet generate --schema s.json --dart lib/a.dart"),
    );
    assert!(
        report.contains("lib/a.dart: does not exist yet"),
        "{report}"
    );
}

#[test]
fn a_stale_report_without_a_command_still_names_the_files() {
    // Defensive: `command` is always `Some` on this path today, and a report
    // that lost its files because it lost its command would be a bad trade.
    let report = failure(&CliError::Stale(vec![differs("a.dart", "x\n")]), None);
    assert!(report.contains("a.dart"), "{report}");
    assert!(!report.contains("Regenerate with"), "{report}");
}

#[test]
fn every_report_ends_with_exactly_one_newline() {
    let reports = [
        failure(&CliError::Usage(UsageError::NoCommand), None),
        failure(&CliError::Stale(vec![differs("a", "x\n")]), Some("cmd")),
        failure(
            &CliError::Emit(duet_codegen::EmitError::TooManyClasses { max: 1 }),
            None,
        ),
    ];
    for report in &reports {
        assert!(report.ends_with('\n'), "{report:?}");
        assert!(!report.ends_with("\n\n"), "{report:?}");
    }
}
