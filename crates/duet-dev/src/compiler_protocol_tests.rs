//! The reverse-engineered line protocol, including shapes this machine has
//! never produced.

use super::*;

/// Feeds lines until the parser completes, returning the terminator and the
/// diagnostics collected.
fn run(lines: &[&str]) -> Option<(Terminator, Vec<String>)> {
    let mut parser = ResultParser::default();
    for line in lines {
        if let Feed::Done(terminator) = parser.feed(line) {
            return Some((terminator, parser.take_diagnostics()));
        }
    }
    None
}

#[test]
fn the_exact_exchange_spike_c_captured() {
    // Verbatim from `spikes/spike-c-macos/FINDINGS.md`: a bare boundary
    // terminator with no trailing dill path or error count. This is what the
    // SDK in this repository actually produces, and any change here would
    // break the real thing.
    let (terminator, diagnostics) = run(&[
        "result ffaa2882-edbf-4170-a6c4-cb390ca8a3c5",
        "ffaa2882-edbf-4170-a6c4-cb390ca8a3c5",
    ])
    .expect("the block should complete");
    assert_eq!(
        terminator,
        Terminator::default(),
        "nothing trailing to read"
    );
    assert!(diagnostics.is_empty(), "a clean compile has no diagnostics");
}

#[test]
fn the_documented_terminator_with_a_dill_path_and_error_count_is_also_read() {
    // What `--help` and `flutter_tools`' own parser expect. Supporting both
    // shapes is what keeps an SDK upgrade from silently breaking error
    // detection.
    let (terminator, _) = run(&[
        "result abc123",
        "abc123 /tmp/duet-dev/out.dill.incremental.dill 0",
    ])
    .expect("the block should complete");
    assert_eq!(
        terminator.dill.as_deref(),
        Some("/tmp/duet-dev/out.dill.incremental.dill")
    );
    assert_eq!(terminator.errors, Some(0));
}

#[test]
fn a_dill_path_containing_spaces_still_parses() {
    // Splitting on the first space instead of the last breaks on any project
    // under `~/My Projects/`, which is not hypothetical on macOS.
    let terminator = Terminator::parse(" /Users/x/My Projects/app/out.dill 3");
    assert_eq!(
        terminator.dill.as_deref(),
        Some("/Users/x/My Projects/app/out.dill")
    );
    assert_eq!(terminator.errors, Some(3));
}

#[test]
fn a_terminator_whose_last_token_is_not_a_count_is_all_path() {
    // Better to report no count — and fall back to reading the diagnostics —
    // than to guess and drop part of a path.
    let terminator = Terminator::parse("/tmp/out.dill");
    assert_eq!(terminator.dill.as_deref(), Some("/tmp/out.dill"));
    assert_eq!(terminator.errors, None);

    let odd = Terminator::parse("/tmp/out.dill maybe");
    assert_eq!(odd.dill.as_deref(), Some("/tmp/out.dill maybe"));
    assert_eq!(odd.errors, None);
}

#[test]
fn stray_output_before_the_result_line_is_discarded() {
    // `accept` sometimes echoes a delayed confirmation that lands in the
    // *next* command's output. Counting it as a diagnostic would make a clean
    // compile look like it said something.
    let (_, diagnostics) = run(&[
        "prior-boundary /tmp/out.dill 1",
        "some unrelated chatter",
        "result xyz",
        "lib/main.dart:3:1: Error: broken",
        "xyz",
    ])
    .expect("the block should complete");
    assert_eq!(
        diagnostics,
        vec!["lib/main.dart:3:1: Error: broken"],
        "only lines after `result` are diagnostics"
    );
}

#[test]
fn an_empty_result_line_does_not_latch_an_empty_boundary() {
    // The nastiest failure mode available: an empty boundary makes *every*
    // subsequent line "start with" it, so the block would terminate on the
    // very next line with no diagnostics and a false success.
    let mut parser = ResultParser::default();
    assert_eq!(parser.feed("result "), Feed::NeedMore);
    assert_eq!(
        parser.feed("this must not be treated as a terminator"),
        Feed::NeedMore,
        "an empty boundary must not swallow the next line"
    );
    assert_eq!(parser.feed("result real-key"), Feed::NeedMore);
    assert!(matches!(parser.feed("real-key"), Feed::Done(_)));
}

#[test]
fn line_endings_are_stripped_from_both_diagnostics_and_terminators() {
    // The reader hands over whatever the pipe produced, and a `\r` on the
    // terminator would stop it matching the boundary at all — a hang.
    let (terminator, diagnostics) = run(&[
        "result k\r\n",
        "lib/main.dart:1:1: Error: nope\r\n",
        "k /tmp/out.dill 1\r\n",
    ])
    .expect("the block should complete");
    assert_eq!(terminator.errors, Some(1));
    assert_eq!(diagnostics, vec!["lib/main.dart:1:1: Error: nope"]);
}

#[test]
fn an_incomplete_block_never_completes() {
    // The parser must not invent a terminator; the caller's deadline is what
    // handles a compiler that stops talking.
    assert!(
        run(&["result k", "lib/main.dart:1:1: Error: x"]).is_none(),
        "a block with no terminator is not done"
    );
    assert!(
        run(&["no result line here", "nor here"]).is_none(),
        "a block that never starts is not done"
    );
}

#[test]
fn error_detection_matches_real_cfe_severities_and_not_other_words() {
    // The fallback used when the compiler gives no error count. Spike C's
    // version also matched "exception" anywhere, case-insensitively, which
    // would classify perfectly good code as broken — and a driver that refuses
    // to reload working code is worse than one that tries to reload broken
    // code, which the VM simply declines.
    let errors = [
        "lib/main.dart:12:8: Error: Expected ';' after this.",
        "Error: Compilation failed",
        "../pkg/lib/a.dart:1:1: Error: whatever",
    ];
    for line in errors {
        assert!(is_error(line), "{line:?} is an error");
    }
    let not_errors = [
        "lib/main.dart:12:8: Warning: unused import",
        "lib/main.dart:12:8: Context: found here",
        "lib/main.dart:12:8: Info: consider const",
        // The exact shape Spike C's heuristic got wrong.
        "lib/main.dart:9:3: Warning: catching FormatException here is broad",
        "throw Exception('boom');",
        "",
    ];
    for line in not_errors {
        assert!(!is_error(line), "{line:?} is not an error");
    }
}

#[test]
fn the_compile_command_is_one_line_with_the_entrypoint() {
    assert_eq!(
        compile_command("package:duet_guest/main.dart"),
        "compile package:duet_guest/main.dart\n"
    );
}

#[test]
fn the_recompile_command_lists_every_invalidated_file_between_two_boundaries() {
    // Spike C only ever invalidated the entrypoint because it only ever edited
    // one file. A real watcher sees edits anywhere, and listing only the
    // entrypoint would make the compiler miss them — the change would compile
    // to nothing and the reload would "succeed" with no effect.
    let command = recompile_command(
        "duet-dev-7",
        &[
            "package:duet_guest/main.dart".to_string(),
            "package:duet_guest/reload_driver.dart".to_string(),
        ],
    );
    assert_eq!(
        command,
        "recompile duet-dev-7\n\
         package:duet_guest/main.dart\n\
         package:duet_guest/reload_driver.dart\n\
         duet-dev-7\n"
    );
}

#[test]
fn a_recompile_with_no_invalidated_files_still_frames_correctly() {
    // The caller substitutes the entrypoint for an empty list, but the
    // command builder must not produce something malformed if it ever sees
    // one — a missing terminator line would hang the exchange.
    assert_eq!(recompile_command("b", &[]), "recompile b\nb\n");
}
