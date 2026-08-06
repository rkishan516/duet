//! The binary itself: argv, exit codes, and the bytes on both streams.
//!
//! `src/*_tests.rs` cover the library through [`duet_cli::execute`], which is
//! where the branching belongs. This drives the **process**, because that is
//! what a user and a CI job actually run, and because the parts only a process
//! has — argv, exit codes, two real streams and a working directory — have no
//! other cover.
//!
//! # The trap this file is shaped around
//!
//! A CLI test that asserts `exit == 0` would pass against a generator that
//! wrote an empty file, the wrong file, or the same file twice. So every
//! successful run here asserts on the **bytes that ended up on disk**, and the
//! `--check` tests assert that it *fails*, with the right code and the right
//! message, on a file that was deliberately broken. A `--check` that cannot
//! fail is worse than no `--check`: it is a green gate over an unguarded seam.
//!
//! `CARGO_BIN_EXE_duet` is set by Cargo for this crate's integration tests, so
//! the binary under test is the one just built rather than whatever is on PATH.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What one run of the binary produced.
struct Run {
    code: i32,
    out: String,
    err: String,
}

/// Runs `duet` with `arguments`, from `directory`.
///
/// The working directory is per-child rather than process-wide, so these tests
/// stay safe to run in parallel — and it matters, because every path a run is
/// given is echoed into the generated header, so a relative path only
/// reproduces the same bytes from the same directory.
fn run_in(directory: &Path, arguments: &[&str]) -> Run {
    let done = Command::new(env!("CARGO_BIN_EXE_duet"))
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the duet binary should start");
    Run {
        code: done.status.code().unwrap_or(-1),
        out: String::from_utf8(done.stdout).expect("duet only ever writes UTF-8"),
        err: String::from_utf8_lossy(&done.stderr).into_owned(),
    }
}

/// Runs `duet` from the repository root.
fn run(arguments: &[&str]) -> Run {
    run_in(&repo_root(), arguments)
}

/// The repository root, found from this crate rather than from the working
/// directory, which `cargo test` does not promise.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The committed schema, and the two clients this repository commits for it.
///
/// `examples/generated/` exists so that the tool's own promise — "run this and
/// commit the result" — has committed bytes to be checked against. Every other
/// generated file in this repository overrides the TypeScript import specifiers
/// to reach into `packages/duet-js/src`, because those goldens live inside that
/// package; these two are generated with the **defaults**, which is what a third
/// party gets and what nothing else here exercises.
const SCHEMA: &str = "schema/app.json";
const DART: &str = "examples/generated/app.duet.dart";
const TS: &str = "examples/generated/app.duet.ts";

/// The canonical invocation for the committed example, less the leading `duet`.
const GENERATE: [&str; 7] = ["generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS];

/// A scratch directory unique to one test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!("duet-cli-e2e-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the scratch directory should be creatable");
        Scratch(path)
    }

    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// A copy of the repository's schema and both committed clients, at the
    /// **same relative paths**.
    ///
    /// Same relative paths on purpose: a generated header names the paths it
    /// was given, so a copy checked from a differently-shaped directory would
    /// report a difference in the header rather than in whatever the test
    /// meant to change. With the layout reproduced, `--check` here is exactly
    /// the check CI runs against the real files, and a test may then break one
    /// of them without touching the repository.
    fn with_committed_example(name: &str) -> Scratch {
        let scratch = Scratch::new(name);
        for relative in [SCHEMA, DART, TS] {
            let destination = scratch.at(relative);
            let parent = destination.parent().expect("every path here has a parent");
            fs::create_dir_all(parent).expect("the layout should be creatable");
            fs::copy(repo_root().join(relative), &destination)
                .unwrap_or_else(|e| panic!("cannot copy {relative}: {e}"));
        }
        scratch
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- help

#[test]
fn help_is_on_stdout_and_explains_the_tool() {
    let run = run(&["--help"]);
    assert_eq!(run.code, 0, "stderr: {}", run.err);
    assert_eq!(run.err, "", "help must not go to stderr");
    assert!(run.out.contains("duet generate --schema"), "{}", run.out);
    assert!(run.out.contains("EXIT CODES"), "{}", run.out);
    assert!(run.out.contains("SharedState"), "{}", run.out);
}

#[test]
fn the_generate_help_lists_every_flag_and_says_what_check_does() {
    let run = run(&["generate", "--help"]);
    assert_eq!(run.code, 0, "stderr: {}", run.err);
    for flag in ["--schema", "--dart", "--ts", "--check"] {
        assert!(run.out.contains(flag), "{flag} missing from:\n{}", run.out);
    }
    assert!(run.out.contains("exit 3"), "{}", run.out);
}

#[test]
fn the_version_is_printed_and_parses_as_a_version() {
    let run = run(&["--version"]);
    assert_eq!(run.code, 0, "stderr: {}", run.err);
    assert_eq!(run.out, format!("duet {}\n", env!("CARGO_PKG_VERSION")));
    assert_eq!(run.err, "");
}

// ------------------------------------------------------------ generate

#[test]
fn generate_writes_the_exact_bytes_the_committed_example_holds() {
    // The assertion that a "exit code 0" test would miss entirely: the bytes on
    // disk, compared against a file this repository commits and CI checks.
    let scratch = Scratch::new("generate");
    fs::create_dir_all(scratch.at("schema")).unwrap();
    fs::copy(repo_root().join(SCHEMA), scratch.at(SCHEMA)).unwrap();

    let run = run_in(&scratch.0, &GENERATE);
    assert_eq!(run.code, 0, "stderr: {}", run.err);
    assert_eq!(
        run.out,
        format!("wrote {DART}\nwrote {TS}\n"),
        "the run must name every file it wrote"
    );
    assert_eq!(run.err, "");

    for relative in [DART, TS] {
        assert_eq!(
            fs::read_to_string(scratch.at(relative)).unwrap(),
            fs::read_to_string(repo_root().join(relative)).unwrap(),
            "{relative} is not byte-identical to the committed client"
        );
    }
}

#[test]
fn generate_creates_the_directories_it_needs() {
    let scratch = Scratch::new("mkdir");
    fs::create_dir_all(scratch.at("schema")).unwrap();
    fs::copy(repo_root().join(SCHEMA), scratch.at(SCHEMA)).unwrap();

    let run = run_in(
        &scratch.0,
        &[
            "generate",
            "--schema",
            SCHEMA,
            "--dart",
            "a/b/c/app.duet.dart",
        ],
    );
    assert_eq!(run.code, 0, "stderr: {}", run.err);
    assert!(scratch.at("a/b/c/app.duet.dart").is_file());
}

#[test]
fn generating_only_one_language_writes_only_one_file() {
    let scratch = Scratch::new("onelang");
    fs::create_dir_all(scratch.at("schema")).unwrap();
    fs::copy(repo_root().join(SCHEMA), scratch.at(SCHEMA)).unwrap();

    let run = run_in(&scratch.0, &["generate", "--schema", SCHEMA, "--ts", TS]);
    assert_eq!(run.code, 0, "stderr: {}", run.err);
    assert_eq!(run.out, format!("wrote {TS}\n"));
    assert!(!scratch.at(DART).exists());
}

// --------------------------------------------------------------- check

#[test]
fn check_passes_against_the_committed_example() {
    // THE staleness gate, run exactly as CI runs it, against the real files.
    let run = run(&[
        "generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS, "--check",
    ]);
    assert_eq!(
        run.code, 0,
        "the committed example is stale; regenerate it.\nstderr: {}",
        run.err
    );
    assert_eq!(run.out, format!("up to date: {DART}\nup to date: {TS}\n"));
    assert_eq!(run.err, "");
}

#[test]
fn check_fails_on_a_mutated_golden_and_names_the_line_and_the_fix() {
    // A `--check` that cannot fail is a green gate over nothing, so this is the
    // load-bearing test in the file. It works on a copy of the repository's own
    // layout rather than on the repository, so the mutation is real — the same
    // bytes CI checks, at the same relative paths, so the generated header
    // matches and the ONLY difference is the one this test introduced — while
    // nothing under version control is ever written to.
    let scratch = Scratch::with_committed_example("mutated");
    let dart = scratch.at(DART);
    let original = fs::read_to_string(&dart).unwrap();
    assert!(
        original.contains("'counter'"),
        "the golden should hold the path literal this test mutates"
    );
    fs::write(&dart, original.replace("'counter'", "'countr'")).unwrap();

    let run = run_in(
        &scratch.0,
        &[
            "generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS, "--check",
        ],
    );

    assert_eq!(
        run.code, 3,
        "a stale file must exit 3.\nstderr: {}",
        run.err
    );
    assert_eq!(run.out, "", "a failure must not report success on stdout");
    assert!(
        run.err.contains("1 generated file(s) are out of date"),
        "{}",
        run.err
    );
    assert!(
        run.err.contains(DART),
        "the file must be named: {}",
        run.err
    );
    // The TypeScript path still appears in the printed fix, which regenerates
    // both; what must not appear is a *report* for it, which is spelled with a
    // trailing colon.
    assert!(
        !run.err.contains(&format!("{TS}:")),
        "the untouched file must not be reported as stale: {}",
        run.err
    );
    assert!(
        run.err.contains("- ") && run.err.contains("+ "),
        "the failure must show a diff: {}",
        run.err
    );
    assert!(
        run.err.contains("'countr'") && run.err.contains("'counter'"),
        "the diff must show both sides: {}",
        run.err
    );
    assert!(
        run.err.contains(&format!(
            "duet generate --schema {SCHEMA} --dart {DART} --ts {TS}"
        )),
        "the failure must print the exact command that fixes it: {}",
        run.err
    );
    assert!(
        !run.err.contains("--check"),
        "the printed fix must not be the invocation that writes nothing: {}",
        run.err
    );
}

#[test]
fn check_leaves_the_stale_file_exactly_as_it_found_it() {
    let scratch = Scratch::with_committed_example("readonly");
    let dart = scratch.at(DART);
    fs::write(&dart, "tampered\n").unwrap();
    let run = run_in(
        &scratch.0,
        &[
            "generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS, "--check",
        ],
    );
    assert_eq!(run.code, 3);
    assert_eq!(
        fs::read_to_string(&dart).unwrap(),
        "tampered\n",
        "--check wrote to the file it was only supposed to report on"
    );
}

#[test]
fn check_reports_a_file_that_does_not_exist_yet() {
    let scratch = Scratch::with_committed_example("absent");
    fs::remove_file(scratch.at(TS)).unwrap();
    let run = run_in(
        &scratch.0,
        &[
            "generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS, "--check",
        ],
    );
    assert_eq!(run.code, 3);
    assert!(
        run.err.contains(&format!("{TS}: does not exist yet")),
        "{}",
        run.err
    );
}

#[test]
fn check_reports_every_stale_file_rather_than_stopping_at_the_first() {
    let scratch = Scratch::with_committed_example("both");
    fs::write(scratch.at(DART), "a\n").unwrap();
    fs::write(scratch.at(TS), "b\n").unwrap();
    let run = run_in(
        &scratch.0,
        &[
            "generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS, "--check",
        ],
    );
    assert_eq!(run.code, 3);
    assert!(run.err.contains("2 generated file(s)"), "{}", run.err);
    assert!(run.err.contains(DART), "{}", run.err);
    assert!(run.err.contains(TS), "{}", run.err);
}

// ---------------------------------------------------- malformed inputs

#[test]
fn a_missing_schema_file_is_a_run_failure_naming_the_path() {
    let run = run(&["generate", "--schema", "schema/absent.json", "--ts", "a.ts"]);
    assert_eq!(run.code, 1, "a failed run is exit 1, not a usage error");
    assert_eq!(run.out, "");
    assert!(run.err.contains("schema/absent.json"), "{}", run.err);
    assert!(run.err.starts_with("duet: cannot read"), "{}", run.err);
}

#[test]
fn a_schema_that_is_not_json_is_refused_with_the_readers_own_message() {
    let scratch = Scratch::new("notjson");
    fs::write(scratch.at("bad.json"), "this is not JSON\n").unwrap();
    let run = run_in(
        &scratch.0,
        &["generate", "--schema", "bad.json", "--ts", "a.ts"],
    );
    assert_eq!(run.code, 1);
    assert!(run.err.contains("not valid JSON"), "{}", run.err);
    assert!(run.err.contains("bad.json"), "{}", run.err);
    assert!(!scratch.at("a.ts").exists(), "a failed run wrote a file");
}

#[test]
fn json_that_is_not_a_schema_is_refused_with_the_place_it_went_wrong() {
    let scratch = Scratch::new("notaschema");
    fs::write(scratch.at("bad.json"), r#"{"version": 1, "types": []}"#).unwrap();
    let run = run_in(
        &scratch.0,
        &["generate", "--schema", "bad.json", "--ts", "a.ts"],
    );
    assert_eq!(run.code, 1);
    assert!(run.err.contains("is not a valid schema"), "{}", run.err);
    assert!(run.err.contains("root"), "{}", run.err);
}

#[test]
fn a_schema_with_no_faithful_spelling_is_refused_after_it_parses() {
    let scratch = Scratch::new("unemittable");
    fs::write(
        scratch.at("scalar.json"),
        r#"{"root": {"kind": "int"}, "types": [], "version": 1}"#,
    )
    .unwrap();
    let run = run_in(
        &scratch.0,
        &["generate", "--schema", "scalar.json", "--dart", "a.dart"],
    );
    assert_eq!(run.code, 1);
    assert!(run.err.contains("named struct at the root"), "{}", run.err);
    assert!(!scratch.at("a.dart").exists());
}

#[test]
fn an_output_path_that_cannot_be_created_fails_without_writing_the_other() {
    // `a.dart` is a plain file, so `a.dart/x.ts` cannot be created. The Dart
    // client is asked for first and must still not appear.
    let scratch = Scratch::new("badoutput");
    fs::create_dir_all(scratch.at("schema")).unwrap();
    fs::copy(repo_root().join(SCHEMA), scratch.at(SCHEMA)).unwrap();
    fs::write(scratch.at("blocker"), "not a directory\n").unwrap();

    let run = run_in(
        &scratch.0,
        &[
            "generate",
            "--schema",
            SCHEMA,
            "--dart",
            "out.dart",
            "--ts",
            "blocker/out.ts",
        ],
    );
    assert_eq!(run.code, 1, "stderr: {}", run.err);
    assert!(run.err.contains("blocker"), "{}", run.err);
    assert!(
        !scratch.at("out.dart").exists(),
        "the Dart client was written even though the run failed"
    );
}

#[test]
fn an_output_that_is_a_directory_says_so_and_names_the_fix() {
    let scratch = Scratch::new("outputisdir");
    fs::create_dir_all(scratch.at("schema")).unwrap();
    fs::copy(repo_root().join(SCHEMA), scratch.at(SCHEMA)).unwrap();
    fs::create_dir_all(scratch.at("lib")).unwrap();

    let run = run_in(
        &scratch.0,
        &["generate", "--schema", SCHEMA, "--dart", "lib"],
    );
    assert_eq!(run.code, 1, "stderr: {}", run.err);
    assert!(run.err.contains("is a directory"), "{}", run.err);
    assert!(run.err.contains("name the file to write"), "{}", run.err);
}

// ------------------------------------------------------- usage failures

#[test]
fn no_arguments_is_a_usage_failure_pointing_at_the_help() {
    let run = run(&[]);
    assert_eq!(run.code, 2, "usage is exit 2, distinct from a failed run");
    assert_eq!(run.out, "");
    assert!(run.err.contains("duet --help"), "{}", run.err);
}

#[test]
fn an_unknown_command_is_a_usage_failure() {
    let run = run(&["compile"]);
    assert_eq!(run.code, 2);
    assert!(run.err.contains("no `compile` command"), "{}", run.err);
}

#[test]
fn asking_for_no_output_is_a_usage_failure_rather_than_a_silent_success() {
    // Exit 0 having written nothing is the worst outcome a generator has.
    let run = run(&["generate", "--schema", SCHEMA]);
    assert_eq!(run.code, 2);
    assert_eq!(run.out, "");
    assert!(run.err.contains("nothing to generate"), "{}", run.err);
}

#[test]
fn a_flag_swallowing_the_next_flag_is_a_usage_failure() {
    let run = run(&["generate", "--schema", SCHEMA, "--dart", "--ts", "a.ts"]);
    assert_eq!(run.code, 2);
    assert!(run.err.contains("looks like a flag"), "{}", run.err);
}

#[test]
fn the_four_exit_codes_are_reachable_and_distinct() {
    // Pinned as a set, because the value of 3 being separate from 1 is that a
    // caller can branch on it — and that stops being true the moment two
    // situations share a code.
    let scratch = Scratch::with_committed_example("codes");
    fs::write(scratch.at(DART), "stale\n").unwrap();
    let check = [
        "generate", "--schema", SCHEMA, "--dart", DART, "--ts", TS, "--check",
    ];

    assert_eq!(run(&["--help"]).code, 0);
    assert_eq!(
        run(&["generate", "--schema", "schema/absent.json", "--ts", "a.ts"]).code,
        1
    );
    assert_eq!(run(&["generate"]).code, 2);
    assert_eq!(run_in(&scratch.0, &check).code, 3);
}
