//! Every arm of the hand-rolled parser.
//!
//! Hand-rolling argv is only defensible if every branch is pinned, because the
//! failure mode of a hand-rolled parser is not a crash — it is accepting
//! something and doing the wrong thing quietly. Each rejection below has a test
//! that would pass if the rejection were removed *and* the tool silently did
//! something else, so the assertion is on the specific error, never on
//! `is_err()`.

use super::*;

/// The parse of `arguments`, or a panic naming what came back instead.
fn generate_of(arguments: &[&str]) -> Generate {
    match parse(arguments) {
        Ok(Invocation::Generate(request)) => request,
        other => panic!("{arguments:?} should parse as a generate: {other:?}"),
    }
}

/// The usage error `arguments` produces, or a panic.
fn usage_of(arguments: &[&str]) -> UsageError {
    match parse(arguments) {
        Err(e) => e,
        Ok(other) => panic!("{arguments:?} should be refused, but parsed as {other:?}"),
    }
}

#[test]
fn the_full_invocation_parses() {
    let request = generate_of(&[
        "generate",
        "--schema",
        "schema/app.json",
        "--dart",
        "lib/app.duet.dart",
        "--ts",
        "src/app.duet.ts",
    ]);
    assert_eq!(request.schema, PathBuf::from("schema/app.json"));
    assert_eq!(request.dart, Some(PathBuf::from("lib/app.duet.dart")));
    assert_eq!(request.ts, Some(PathBuf::from("src/app.duet.ts")));
    assert!(!request.check);
}

#[test]
fn either_output_may_be_asked_for_alone() {
    let dart = generate_of(&["generate", "--schema", "s.json", "--dart", "a.dart"]);
    assert_eq!(dart.dart, Some(PathBuf::from("a.dart")));
    assert_eq!(dart.ts, None);

    let ts = generate_of(&["generate", "--schema", "s.json", "--ts", "a.ts"]);
    assert_eq!(ts.dart, None);
    assert_eq!(ts.ts, Some(PathBuf::from("a.ts")));
}

#[test]
fn check_is_a_flag_with_no_value() {
    let request = generate_of(&["generate", "--check", "--schema", "s.json", "--ts", "a.ts"]);
    assert!(request.check);
}

#[test]
fn a_flag_may_carry_its_value_after_an_equals_sign() {
    let request = generate_of(&["generate", "--schema=s.json", "--dart=a.dart"]);
    assert_eq!(request.schema, PathBuf::from("s.json"));
    assert_eq!(request.dart, Some(PathBuf::from("a.dart")));
}

#[test]
fn an_equals_form_is_how_a_path_starting_with_a_dash_is_written() {
    // The escape hatch the `FlagShapedValue` rejection points at. Without it
    // there would be no way to name such a file at all.
    let request = generate_of(&["generate", "--schema=s.json", "--dart=--odd.dart"]);
    assert_eq!(request.dart, Some(PathBuf::from("--odd.dart")));
}

#[test]
fn flag_order_does_not_matter() {
    let a = generate_of(&["generate", "--dart", "a.dart", "--schema", "s.json"]);
    let b = generate_of(&["generate", "--schema", "s.json", "--dart", "a.dart"]);
    assert_eq!(a, b);
}

#[test]
fn help_and_version_are_reachable_three_ways_each() {
    for spelling in ["-h", "--help", "help"] {
        assert_eq!(parse(&[spelling]), Ok(Invocation::Help), "{spelling}");
    }
    for spelling in ["-V", "--version", "version"] {
        assert_eq!(parse(&[spelling]), Ok(Invocation::Version), "{spelling}");
    }
}

#[test]
fn generate_has_its_own_help_and_it_wins_over_a_missing_schema() {
    // `duet generate --help` must print help rather than complain that
    // `--schema` is missing, which is precisely what a user asking for help
    // does not know yet.
    for spelling in ["-h", "--help"] {
        assert_eq!(
            parse(&["generate", spelling]),
            Ok(Invocation::GenerateHelp),
            "{spelling}"
        );
    }
}

#[test]
fn no_arguments_at_all_points_at_the_help() {
    let error = usage_of(&[]);
    assert_eq!(error, UsageError::NoCommand);
    assert!(error.to_string().contains("duet --help"), "{error}");
}

#[test]
fn an_unknown_command_names_the_only_one_there_is() {
    let error = usage_of(&["build"]);
    assert_eq!(
        error,
        UsageError::UnknownCommand {
            found: "build".to_string()
        }
    );
    assert!(error.to_string().contains("generate"), "{error}");
}

#[test]
fn an_unknown_flag_lists_the_flags_that_exist() {
    let error = usage_of(&["generate", "--swift", "a.swift"]);
    assert_eq!(
        error,
        UsageError::UnknownFlag {
            command: "generate",
            found: "--swift".to_string()
        }
    );
    let message = error.to_string();
    for flag in ["--schema", "--dart", "--ts", "--check"] {
        assert!(message.contains(flag), "{message} omits {flag}");
    }
}

#[test]
fn a_positional_argument_is_refused_rather_than_guessed_at() {
    // `duet generate schema/app.json` is the natural first guess, and silently
    // treating it as `--schema` would make the flags optional by accident.
    let error = usage_of(&["generate", "schema/app.json"]);
    assert_eq!(
        error,
        UsageError::UnexpectedArgument {
            command: "generate",
            found: "schema/app.json".to_string()
        }
    );
    assert!(error.to_string().contains("flag"), "{error}");
}

#[test]
fn a_lone_dash_is_a_positional_argument_not_a_flag() {
    let error = usage_of(&["generate", "-"]);
    assert_eq!(
        error,
        UsageError::UnexpectedArgument {
            command: "generate",
            found: "-".to_string()
        }
    );
}

#[test]
fn a_repeated_flag_is_refused_rather_than_letting_the_last_win() {
    for arguments in [
        vec!["generate", "--schema", "a.json", "--schema", "b.json"],
        vec![
            "generate", "--schema", "s.json", "--dart", "a", "--dart", "b",
        ],
        vec!["generate", "--schema", "s.json", "--ts", "a", "--ts", "b"],
        vec![
            "generate", "--schema", "s.json", "--ts", "a", "--check", "--check",
        ],
    ] {
        let error = usage_of(&arguments);
        assert!(
            matches!(error, UsageError::RepeatedFlag { .. }),
            "{arguments:?} gave {error:?}"
        );
        assert!(error.to_string().contains("more than once"), "{error}");
    }
}

#[test]
fn a_flag_at_the_end_of_argv_says_what_it_wanted() {
    let error = usage_of(&["generate", "--schema"]);
    assert_eq!(error, UsageError::MissingValue { flag: "--schema" });
    assert!(error.to_string().contains("path"), "{error}");
}

#[test]
fn a_flag_shaped_value_is_refused_and_the_message_shows_the_escape() {
    // Without this, `duet generate --schema s.json --dart --ts out.ts` writes a
    // Dart file called `--ts` and never mentions TypeScript again.
    let error = usage_of(&["generate", "--schema", "s.json", "--dart", "--ts", "out.ts"]);
    assert_eq!(
        error,
        UsageError::FlagShapedValue {
            flag: "--dart",
            found: "--ts".to_string()
        }
    );
    assert!(error.to_string().contains("--dart=--ts"), "{error}");
}

#[test]
fn a_value_on_check_is_refused_rather_than_ignored() {
    // `--check=false` is a natural thing to try, and quietly enabling `--check`
    // for it would turn a request to write files into one that refuses to.
    let error = usage_of(&[
        "generate",
        "--schema",
        "s.json",
        "--ts",
        "a.ts",
        "--check=false",
    ]);
    assert_eq!(error, UsageError::UnexpectedValue { flag: "--check" });
    assert!(error.to_string().contains("takes no value"), "{error}");
}

#[test]
fn the_unknown_flag_message_lists_exactly_the_flags_the_parser_takes() {
    // The list in the message is written out; this is what stops it drifting
    // from `PATH_FLAGS` when a flag is added or removed.
    let message = UsageError::UnknownFlag {
        command: "generate",
        found: "--x".to_string(),
    }
    .to_string();
    for flag in PATH_FLAGS {
        assert!(message.contains(flag), "{message} omits {flag}");
    }
    assert!(message.contains("--check"), "{message}");
}

#[test]
fn every_path_flag_is_accepted_and_none_of_them_collides() {
    // Each flag must reach its own slot; a copy-paste in `set_path` would
    // otherwise send two flags to one field and the run would generate the
    // wrong language.
    let request = generate_of(&["generate", "--schema=s.json", "--dart=d.dart", "--ts=t.ts"]);
    assert_eq!(request.schema, PathBuf::from("s.json"));
    assert_eq!(request.dart, Some(PathBuf::from("d.dart")));
    assert_eq!(request.ts, Some(PathBuf::from("t.ts")));
}

#[test]
fn a_missing_schema_is_named_specifically() {
    let error = usage_of(&["generate", "--dart", "a.dart"]);
    assert_eq!(error, UsageError::NoSchema);
    assert!(error.to_string().contains("--schema"), "{error}");
}

#[test]
fn asking_for_no_output_is_refused_rather_than_succeeding_silently() {
    // The worst possible success: exit 0, nothing written, nothing said.
    let error = usage_of(&["generate", "--schema", "s.json"]);
    assert_eq!(error, UsageError::NoOutput);
    let message = error.to_string();
    assert!(message.contains("--dart"), "{message}");
    assert!(message.contains("--ts"), "{message}");
}

#[test]
fn the_command_is_canonical_rather_than_the_argv_that_produced_it() {
    // Load-bearing: this string is written into every generated file's header,
    // so two orderings of the same request must produce byte-identical output.
    let a = generate_of(&[
        "generate",
        "--ts",
        "src/a.ts",
        "--dart",
        "lib/a.dart",
        "--schema",
        "schema/app.json",
    ]);
    let b = generate_of(&[
        "generate",
        "--schema",
        "schema/app.json",
        "--dart",
        "lib/a.dart",
        "--ts",
        "src/a.ts",
    ]);
    assert_eq!(
        a.command(),
        "duet generate --schema schema/app.json --dart lib/a.dart --ts src/a.ts"
    );
    assert_eq!(a.command(), b.command());
}

#[test]
fn the_command_never_carries_check() {
    // A header saying "regenerate with `... --check`" would name the one
    // invocation that writes nothing, and `--check` would then never match a
    // file any run produced.
    let checked = generate_of(&["generate", "--schema", "s.json", "--ts", "a.ts", "--check"]);
    let plain = generate_of(&["generate", "--schema", "s.json", "--ts", "a.ts"]);
    assert_eq!(checked.command(), plain.command());
    assert!(!checked.command().contains("--check"));
}

#[test]
fn the_command_names_only_the_outputs_that_were_asked_for() {
    let request = generate_of(&["generate", "--schema", "s.json", "--dart", "a.dart"]);
    assert_eq!(
        request.command(),
        "duet generate --schema s.json --dart a.dart"
    );
}

#[test]
fn a_path_that_needs_quoting_gets_it() {
    let request = generate_of(&["generate", "--schema=my schema.json", "--dart=a.dart"]);
    assert_eq!(
        request.command(),
        "duet generate --schema 'my schema.json' --dart a.dart"
    );
}

#[test]
fn an_embedded_quote_survives_the_quoting() {
    let request = generate_of(&["generate", "--schema=it's.json", "--dart=a.dart"]);
    // The POSIX idiom: close, escaped quote, reopen.
    assert!(
        request.command().contains(r#"'it'\''s.json'"#),
        "{}",
        request.command()
    );
}

#[test]
fn an_empty_path_is_quoted_rather_than_vanishing() {
    // `--dart=` is a mistake, but the command echoed back must still be
    // parseable rather than silently dropping an argument.
    let request = generate_of(&["generate", "--schema=s.json", "--dart="]);
    assert_eq!(request.command(), "duet generate --schema s.json --dart ''");
}

#[test]
fn a_long_argument_is_not_echoed_whole() {
    let long = "-".to_string() + &"x".repeat(10_000);
    let error = usage_of(&["generate", &long]);
    let message = error.to_string();
    assert!(message.len() < 300, "{} bytes echoed", message.len());
    assert!(message.contains('…'), "{message}");
}

#[test]
fn a_multibyte_argument_is_cut_on_a_character_boundary() {
    // The truncation runs on hostile input, so it must not be able to panic on
    // a byte index inside a code point.
    let long = format!("--{}", "é".repeat(500));
    let error = usage_of(&["generate", &long]);
    assert!(error.to_string().contains('…'));
}

#[test]
fn every_usage_error_says_something_and_none_of_them_panics() {
    let errors = [
        UsageError::NoCommand,
        UsageError::UnknownCommand {
            found: "x".to_string(),
        },
        UsageError::UnknownFlag {
            command: "generate",
            found: "--x".to_string(),
        },
        UsageError::UnexpectedArgument {
            command: "generate",
            found: "x".to_string(),
        },
        UsageError::RepeatedFlag { flag: "--ts" },
        UsageError::MissingValue { flag: "--ts" },
        UsageError::UnexpectedValue { flag: "--check" },
        UsageError::FlagShapedValue {
            flag: "--ts",
            found: "--dart".to_string(),
        },
        UsageError::NoSchema,
        UsageError::NoOutput,
        UsageError::NoProject,
        UsageError::NoHostCommand,
    ];
    for error in &errors {
        assert!(!error.to_string().is_empty(), "{error:?}");
        assert!(std::error::Error::source(error).is_none());
    }
}

// ===================== duet dev =====================

/// The parse of `arguments`, or a panic naming what came back instead.
fn dev_of(arguments: &[&str]) -> Dev {
    match parse(arguments) {
        Ok(Invocation::Dev(request)) => request,
        other => panic!("{arguments:?} should parse as a dev: {other:?}"),
    }
}

#[test]
fn the_canonical_dev_invocation_parses() {
    let request = dev_of(&[
        "dev",
        "--flutter",
        "fixtures/duet_guest",
        "--flutter-root",
        "/opt/flutter",
        "--",
        "cargo",
        "run",
    ]);
    assert_eq!(request.project, PathBuf::from("fixtures/duet_guest"));
    assert_eq!(request.flutter_root, Some(PathBuf::from("/opt/flutter")));
    assert_eq!(request.entrypoint, None);
    assert_eq!(request.host, vec!["cargo".to_string(), "run".to_string()]);
}

#[test]
fn everything_after_the_separator_belongs_to_the_host_even_when_it_looks_like_a_flag() {
    // The whole reason for `--`. A host command routinely carries its own
    // flags, and `--release` or `--flutter` reaching this parser would be
    // rejected as unknown — or, far worse, silently consumed.
    let request = dev_of(&[
        "dev",
        "--flutter",
        "app",
        "--",
        "cargo",
        "run",
        "--release",
        "--flutter",
        "-h",
        "--",
    ]);
    assert_eq!(request.project, PathBuf::from("app"));
    assert_eq!(
        request.host,
        vec!["cargo", "run", "--release", "--flutter", "-h", "--"]
    );
}

#[test]
fn a_host_argument_containing_spaces_survives_as_one_argument() {
    // The reason `host` is a `Vec` rather than a string to be split: a path
    // with a space in it is normal on macOS, and splitting would break it in a
    // way no quoting the developer tried could fix.
    let request = dev_of(&["dev", "--flutter", "app", "--", "/My Apps/host", "--x y"]);
    assert_eq!(request.host, vec!["/My Apps/host", "--x y"]);
}

#[test]
fn the_entrypoint_can_be_overridden() {
    let request = dev_of(&[
        "dev",
        "--flutter",
        "app",
        "--entrypoint",
        "package:app/other.dart",
        "--",
        "true",
    ]);
    assert_eq!(
        request.entrypoint,
        Some("package:app/other.dart".to_string())
    );
}

#[test]
fn dev_flags_accept_the_inline_form_too() {
    // `--flag=value` is the only way to spell a path that begins with `-`, so
    // it has to work here for the same reason it works for `generate`.
    let request = dev_of(&["dev", "--flutter=app", "--flutter-root=/opt/f", "--", "x"]);
    assert_eq!(request.project, PathBuf::from("app"));
    assert_eq!(request.flutter_root, Some(PathBuf::from("/opt/f")));
}

#[test]
fn dev_without_a_project_is_refused() {
    assert_eq!(
        usage_of(&["dev", "--", "cargo", "run"]),
        UsageError::NoProject
    );
}

#[test]
fn dev_without_a_host_command_is_refused_both_ways_of_omitting_it() {
    // A `duet dev` with nothing to run would start a compiler, watch a
    // directory, and never reload anything — a session that looks like it is
    // working and is not.
    assert_eq!(
        usage_of(&["dev", "--flutter", "app"]),
        UsageError::NoHostCommand,
        "no separator at all"
    );
    assert_eq!(
        usage_of(&["dev", "--flutter", "app", "--"]),
        UsageError::NoHostCommand,
        "a separator with nothing after it"
    );
}

#[test]
fn a_repeated_dev_flag_is_refused() {
    // Same reasoning as `generate`: taking the last silently discards the
    // first, which is a footgun in a script that appends arguments.
    for (arguments, flag) in [
        (
            vec!["dev", "--flutter", "a", "--flutter", "b", "--", "x"],
            "--flutter",
        ),
        (
            vec![
                "dev",
                "--flutter",
                "a",
                "--flutter-root",
                "x",
                "--flutter-root",
                "y",
                "--",
                "x",
            ],
            "--flutter-root",
        ),
        (
            vec![
                "dev",
                "--flutter",
                "a",
                "--entrypoint",
                "p:a/b.dart",
                "--entrypoint",
                "p:a/c.dart",
                "--",
                "x",
            ],
            "--entrypoint",
        ),
    ] {
        assert_eq!(
            usage_of(&arguments),
            UsageError::RepeatedFlag { flag },
            "{arguments:?}"
        );
    }
}

#[test]
fn a_flag_dev_does_not_define_is_refused_and_the_message_names_dev() {
    // A `generate` flag typed into `dev` is the likeliest mistake, and a
    // message listing `generate`'s flags would be actively confusing.
    let error = usage_of(&["dev", "--flutter", "a", "--schema", "s.json", "--", "x"]);
    assert_eq!(
        error,
        UsageError::UnknownFlag {
            command: "dev",
            found: "--schema".to_string()
        }
    );
    let message = error.to_string();
    assert!(message.contains("`dev`"), "got {message}");
    assert!(
        message.contains("--flutter"),
        "it should list dev's own flags: {message}"
    );
    assert!(
        !message.contains("--schema flag; it takes --schema"),
        "it must not list generate's flags: {message}"
    );
}

#[test]
fn a_positional_argument_before_the_separator_is_refused_and_points_at_it() {
    // The most likely spelling mistake: forgetting `--` entirely. The message
    // has to say so, or the developer will try quoting instead.
    let error = usage_of(&["dev", "--flutter", "a", "cargo", "run"]);
    assert_eq!(
        error,
        UsageError::UnexpectedArgument {
            command: "dev",
            found: "cargo".to_string()
        }
    );
    assert!(
        error.to_string().contains("`--`"),
        "the message should point at the separator: {error}"
    );
}

#[test]
fn dev_help_is_reachable_both_ways_and_wins_over_a_missing_project() {
    // `duet dev --help` must print help, not complain that `--flutter` is
    // missing — which is exactly what somebody reaching for help does not have.
    for arguments in [
        vec!["dev", "--help"],
        vec!["dev", "-h"],
        vec!["dev", "--flutter", "a", "--help"],
    ] {
        assert_eq!(
            parse(&arguments),
            Ok(Invocation::DevHelp),
            "{arguments:?} should print the dev help"
        );
    }
}

#[test]
fn the_unknown_command_message_names_both_commands() {
    // It named only `generate` before `dev` existed; a message that still did
    // would hide the new command from everyone who mistyped it.
    let message = UsageError::UnknownCommand {
        found: "develop".to_string(),
    }
    .to_string();
    assert!(message.contains("generate"), "got {message}");
    assert!(message.contains("dev"), "got {message}");
}
