//! Every error names its stage, and says something useful.

use super::*;

fn every_variant() -> Vec<DevError> {
    vec![
        DevError::Timeout {
            stage: Stage::ReloadSources,
            after: Duration::from_secs(30),
        },
        DevError::CompilerExited {
            stage: Stage::Recompile,
            status: Some(70),
            stderr: "Unhandled exception: bad sdk root".to_string(),
        },
        DevError::CompilerExited {
            stage: Stage::BaselineCompile,
            status: None,
            stderr: String::new(),
        },
        DevError::protocol(Stage::Recompile, "no result line in 400 lines of output"),
        DevError::vm(Stage::FindIsolate, "getVM reported 0 isolate(s)"),
        DevError::NotFound {
            stage: Stage::LocateSdk,
            what: "dartaotruntime",
            path: "/nope/bin/dartaotruntime".to_string(),
        },
        DevError::Io {
            stage: Stage::SpawnCompiler,
            doing: "starting frontend_server",
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        },
    ]
}

#[test]
fn every_error_reports_the_stage_it_was_built_with() {
    // The invariant this module exists for. If a variant is added without a
    // stage, this stops compiling — which is the intent.
    let expected = [
        Stage::ReloadSources,
        Stage::Recompile,
        Stage::BaselineCompile,
        Stage::Recompile,
        Stage::FindIsolate,
        Stage::LocateSdk,
        Stage::SpawnCompiler,
    ];
    for (error, want) in every_variant().into_iter().zip(expected) {
        assert_eq!(error.stage(), want, "{error:?} reports the wrong stage");
    }
}

#[test]
fn every_message_leads_with_the_stage_in_brackets() {
    // So a log line is greppable and the first thing read is the location.
    for error in every_variant() {
        let text = error.to_string();
        assert!(
            text.starts_with(&format!("[{}]", error.stage())),
            "{text:?} should lead with its stage"
        );
        assert!(
            text.len() > text.find(']').unwrap_or(0) + 8,
            "{text:?} says almost nothing after the stage"
        );
    }
}

#[test]
fn a_compiler_death_reports_the_status_and_the_stderr_that_explains_it() {
    let error = DevError::CompilerExited {
        stage: Stage::Recompile,
        status: Some(253),
        stderr: "Error: Could not resolve dart:core".to_string(),
    };
    let text = error.to_string();
    assert!(text.contains("253"), "the exit status matters: {text}");
    assert!(
        text.contains("Could not resolve dart:core"),
        "the stderr is where the reason lives: {text}"
    );
}

#[test]
fn a_compiler_death_with_nothing_to_report_still_says_so() {
    // Silence is itself informative — it usually means the process was killed
    // rather than that it failed — so it must not render as a dangling colon.
    let text = DevError::CompilerExited {
        stage: Stage::Accept,
        status: None,
        stderr: String::new(),
    }
    .to_string();
    assert!(text.contains("nothing to stderr"), "got {text}");
    assert!(text.contains("not collected"), "got {text}");
}

#[test]
fn only_the_io_variant_exposes_a_source() {
    // `source()` is what `{:#}`-style error chains walk. Returning a source
    // for variants that have none would duplicate text already in `Display`.
    for error in every_variant() {
        let has_source = std::error::Error::source(&error).is_some();
        assert_eq!(
            has_source,
            matches!(error, DevError::Io { .. }),
            "{error:?} disagrees about having a source"
        );
    }
}

#[test]
fn stage_names_are_stable_lowercase_slugs() {
    // These are what a developer greps for and what a `--json` mode would
    // emit, so they are asserted rather than derived from Debug.
    let cases = [
        (Stage::LocateSdk, "locate-sdk"),
        (Stage::SpawnCompiler, "spawn-compiler"),
        (Stage::BaselineCompile, "baseline-compile"),
        (Stage::LocateVmService, "locate-vm-service"),
        (Stage::Connect, "connect"),
        (Stage::FindIsolate, "find-isolate"),
        (Stage::Recompile, "recompile"),
        (Stage::Accept, "accept"),
        (Stage::ReloadSources, "reload-sources"),
        (Stage::Reassemble, "reassemble"),
        (Stage::Observe, "observe"),
        (Stage::Watch, "watch"),
    ];
    for (stage, want) in cases {
        assert_eq!(stage.name(), want);
        assert_eq!(stage.to_string(), want, "Display should match name()");
        assert!(
            want.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "{want} should be a lower-case slug"
        );
    }
}

#[test]
fn long_detail_is_truncated_but_short_detail_is_left_alone() {
    // A pathological Dart file can produce megabytes of diagnostics. Folding
    // all of it into an error would make every `?` a large copy and every log
    // line unreadable — but truncating a normal message would lose the point.
    let short = "a normal message";
    assert_eq!(truncate(short), short);

    let long = "e".repeat(MAX_DETAIL * 4);
    let cut = truncate(&long);
    assert!(cut.len() < long.len(), "it should be shorter");
    assert!(cut.contains('…'), "it should say it was cut: {cut:.60}");
    assert!(
        cut.contains(&long.len().to_string()),
        "and report the real size"
    );
}

#[test]
fn truncation_never_splits_a_character() {
    // Dart diagnostics contain non-ASCII routinely. Slicing mid-codepoint
    // would panic inside error construction, which is the worst place for it.
    for filler in ["é", "☕", "𝄞"] {
        let text = filler.repeat(MAX_DETAIL);
        let cut = truncate(&text);
        assert!(
            cut.chars().count() > 0,
            "truncating {filler:?} should produce something"
        );
        // Reaching here at all proves no panic; that it is still valid UTF-8
        // is guaranteed by it being a String.
        assert!(cut.is_char_boundary(0));
    }
}

#[test]
fn the_constructors_truncate_what_they_are_given() {
    // `protocol` and `vm` are the two paths that fold peer-supplied text into
    // an error, so they are the two that must bound it.
    let huge = "z".repeat(MAX_DETAIL * 2);
    for error in [
        DevError::protocol(Stage::Recompile, huge.clone()),
        DevError::vm(Stage::ReloadSources, huge),
    ] {
        assert!(
            error.to_string().len() < MAX_DETAIL * 2,
            "{} was not bounded",
            error.stage()
        );
    }
}
