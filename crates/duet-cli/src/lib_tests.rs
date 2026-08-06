//! Which stream each outcome lands on, and which exit code goes with it.

use super::*;

#[test]
fn help_goes_to_stdout_and_succeeds() {
    // stdout rather than stderr: `duet --help | less` is what people do, and a
    // help text on stderr makes that print nothing.
    for spelling in ["--help", "-h", "help"] {
        let outcome = execute(&[spelling]);
        assert_eq!(outcome.code, 0, "{spelling}");
        assert_eq!(outcome.out, help::HELP, "{spelling}");
        assert_eq!(outcome.err, "", "{spelling}");
    }
}

#[test]
fn the_generate_help_is_its_own_text() {
    let outcome = execute(&["generate", "--help"]);
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.out, help::GENERATE_HELP);
    assert_ne!(outcome.out, help::HELP);
}

#[test]
fn the_version_goes_to_stdout_and_succeeds() {
    let outcome = execute(&["--version"]);
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.out, help::version());
    assert_eq!(outcome.err, "");
}

#[test]
fn a_usage_failure_goes_to_stderr_with_nothing_on_stdout() {
    // stdout stays clean so a caller redirecting it does not collect a
    // diagnostic where it expected a report.
    let outcome = execute(&["generate"]);
    assert_eq!(outcome.code, EXIT_USAGE);
    assert_eq!(outcome.out, "");
    assert!(
        outcome.err.contains("--schema is required"),
        "{}",
        outcome.err
    );
}

#[test]
fn an_empty_command_line_is_a_usage_failure() {
    let outcome = execute::<String>(&[]);
    assert_eq!(outcome.code, EXIT_USAGE);
    assert!(outcome.err.contains("duet --help"), "{}", outcome.err);
}

#[test]
fn a_failing_run_reports_the_canonical_command_not_the_argv() {
    // The staleness message tells a developer what to run. It must be the
    // canonical spelling, because that is what the file headers say too.
    let outcome = execute(&[
        "generate",
        "--ts",
        "/nonexistent/duet/a.ts",
        "--schema",
        "/nonexistent/duet/absent.json",
        "--check",
    ]);
    assert_eq!(outcome.code, EXIT_FAILED);
    assert_eq!(outcome.out, "");
    assert!(outcome.err.contains("absent.json"), "{}", outcome.err);
}

#[test]
fn nothing_ever_lands_on_both_streams_at_once() {
    let invocations: Vec<Vec<&str>> = vec![
        vec!["--help"],
        vec!["--version"],
        vec!["generate"],
        vec!["nonsense"],
        vec![
            "generate",
            "--schema",
            "/nonexistent/duet/absent.json",
            "--ts",
            "/nonexistent/duet/a.ts",
        ],
    ];
    for arguments in &invocations {
        let outcome = execute(arguments);
        assert!(
            outcome.out.is_empty() || outcome.err.is_empty(),
            "{arguments:?} wrote to both streams"
        );
    }
}

#[test]
fn every_message_ends_with_exactly_one_newline() {
    let invocations: Vec<Vec<&str>> = vec![
        vec!["--help"],
        vec!["--version"],
        vec!["generate", "--help"],
        vec!["generate"],
        vec!["nonsense"],
    ];
    for arguments in &invocations {
        let outcome = execute(arguments);
        let text = if outcome.out.is_empty() {
            &outcome.err
        } else {
            &outcome.out
        };
        assert!(text.ends_with('\n'), "{arguments:?}: {text:?}");
        assert!(!text.ends_with("\n\n"), "{arguments:?}: {text:?}");
    }
}

#[test]
fn no_input_can_make_execute_panic() {
    // Totality at the entry point, in one place. Each of these has reached a
    // panic in some version of some CLI: an empty flag value, a lone dash, an
    // argument that is only dashes, and a path that cannot exist.
    let cases: Vec<Vec<&str>> = vec![
        vec![""],
        vec!["-"],
        vec!["--"],
        vec!["---"],
        vec!["generate", "--schema="],
        vec!["generate", "--schema=", "--dart="],
        vec!["generate", "--schema", "/", "--dart", "/"],
        vec!["generate", "--schema", "\0", "--dart", "a"],
        vec!["generate", "--schema", "é", "--dart", "é"],
    ];
    for arguments in &cases {
        let outcome = execute(arguments);
        assert_ne!(outcome.code, 0, "{arguments:?} should not succeed");
    }
}
