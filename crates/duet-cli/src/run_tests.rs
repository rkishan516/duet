//! The generate/check behaviour, against real files.
//!
//! `tests/cli.rs` drives the binary and asserts the bytes a user sees. This
//! covers the arms that need a filesystem arranged in a specific way — an
//! unreadable output, a schema that is not UTF-8 — which are awkward to set up
//! through a process and are not about argv at all.

use std::path::PathBuf;

use super::*;

/// A scratch directory unique to one test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!("duet-cli-run-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the scratch directory should be creatable");
        Scratch(path)
    }

    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes a minimal valid schema and returns its path.
    fn schema(&self) -> PathBuf {
        let path = self.at("app.json");
        fs::write(&path, VALID_SCHEMA).expect("the schema should be writable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The smallest schema that emits: one named root with one field.
const VALID_SCHEMA: &str = r#"{
  "root": {"kind": "named", "name": "App"},
  "types": [
    {"fields": [{"key": "counter", "type": {"kind": "int"}}], "name": "App"}
  ],
  "version": 1
}"#;

/// A request generating both languages under `scratch`.
fn request(scratch: &Scratch, check: bool) -> Generate {
    Generate {
        schema: scratch.schema(),
        dart: Some(scratch.at("a.duet.dart")),
        ts: Some(scratch.at("a.duet.ts")),
        check,
    }
}

#[test]
fn a_run_writes_both_files_and_reports_both_paths() {
    let scratch = Scratch::new("write");
    let report = run(&request(&scratch, false)).expect("a valid schema should generate");
    assert_eq!(report.lines.len(), 2);
    assert!(report.lines[0].starts_with("wrote "), "{:?}", report.lines);
    assert!(
        fs::read_to_string(scratch.at("a.duet.dart"))
            .unwrap()
            .contains("counter")
    );
    assert!(
        fs::read_to_string(scratch.at("a.duet.ts"))
            .unwrap()
            .contains("counter")
    );
}

#[test]
fn the_header_names_the_schema_and_the_canonical_command() {
    let scratch = Scratch::new("header");
    let request = request(&scratch, false);
    run(&request).unwrap();
    let dart = fs::read_to_string(scratch.at("a.duet.dart")).unwrap();
    assert!(
        dart.contains(&request.schema.to_string_lossy().into_owned()),
        "{dart}"
    );
    assert!(dart.contains(&request.command()), "{dart}");
}

#[test]
fn check_passes_on_what_a_plain_run_just_wrote() {
    // The round trip that the CI step depends on: generate, then check, with
    // nothing in between. If the two disagreed, `--check` could never pass.
    let scratch = Scratch::new("roundtrip");
    run(&request(&scratch, false)).unwrap();
    let report = run(&request(&scratch, true)).expect("a fresh generation should be up to date");
    assert_eq!(report.lines.len(), 2);
    assert!(
        report.lines[0].starts_with("up to date: "),
        "{:?}",
        report.lines
    );
}

#[test]
fn check_with_the_flags_reordered_still_passes() {
    // Because `Generate::command` is canonical rather than argv-shaped. If it
    // were not, a script that ordered its flags differently from the one that
    // generated the file would report a difference nobody caused.
    let scratch = Scratch::new("reorder");
    run(&request(&scratch, false)).unwrap();
    let reordered = Generate {
        schema: scratch.at("app.json"),
        ts: Some(scratch.at("a.duet.ts")),
        dart: Some(scratch.at("a.duet.dart")),
        check: true,
    };
    run(&reordered).expect("flag order must not affect the bytes");
}

#[test]
fn check_writes_nothing_at_all() {
    let scratch = Scratch::new("readonly");
    run(&request(&scratch, false)).unwrap();
    fs::write(scratch.at("a.duet.dart"), "tampered\n").unwrap();
    let error = run(&request(&scratch, true)).expect_err("a tampered file should be stale");
    assert_eq!(error.exit_code(), crate::error::EXIT_STALE);
    assert_eq!(
        fs::read_to_string(scratch.at("a.duet.dart")).unwrap(),
        "tampered\n",
        "--check repaired the file it was only supposed to report on"
    );
}

#[test]
fn a_stale_file_is_reported_with_a_diff_and_the_other_is_not() {
    let scratch = Scratch::new("stale");
    run(&request(&scratch, false)).unwrap();
    let dart = scratch.at("a.duet.dart");
    let tampered = fs::read_to_string(&dart)
        .unwrap()
        .replace("counter", "countr");
    fs::write(&dart, tampered).unwrap();

    match run(&request(&scratch, true)) {
        Err(CliError::Stale(differences)) => {
            assert_eq!(differences.len(), 1, "{differences:?}");
            assert_eq!(differences[0].path, dart);
            assert!(!differences[0].missing);
            assert!(
                differences[0].diff.contains("counter"),
                "{}",
                differences[0].diff
            );
        }
        other => panic!("expected a staleness report, got {other:?}"),
    }
}

#[test]
fn a_file_that_was_never_generated_is_missing_rather_than_an_io_failure() {
    // `--check` on a fresh clone whose clients were never committed should say
    // what to run, not report ENOENT.
    let scratch = Scratch::new("missing");
    match run(&request(&scratch, true)) {
        Err(CliError::Stale(differences)) => {
            assert_eq!(differences.len(), 2);
            assert!(differences.iter().all(|d| d.missing), "{differences:?}");
        }
        other => panic!("expected a staleness report, got {other:?}"),
    }
}

#[test]
fn a_missing_schema_names_the_path_and_writes_nothing() {
    let scratch = Scratch::new("noschema");
    let request = Generate {
        schema: scratch.at("absent.json"),
        dart: Some(scratch.at("a.dart")),
        ts: None,
        check: false,
    };
    match run(&request) {
        Err(CliError::ReadSchema { path, .. }) => assert_eq!(path, request.schema),
        other => panic!("expected a read failure, got {other:?}"),
    }
    assert!(!scratch.at("a.dart").exists(), "a failed run wrote a file");
}

#[test]
fn a_schema_that_is_not_json_names_the_file_and_writes_nothing() {
    let scratch = Scratch::new("notjson");
    let schema = scratch.at("app.json");
    fs::write(&schema, "this is not JSON").unwrap();
    let request = Generate {
        schema: schema.clone(),
        dart: Some(scratch.at("a.dart")),
        ts: Some(scratch.at("a.ts")),
        check: false,
    };
    match run(&request) {
        Err(CliError::Schema { path, source }) => {
            assert_eq!(path, schema);
            assert!(source.to_string().contains("not valid JSON"), "{source}");
        }
        other => panic!("expected a schema failure, got {other:?}"),
    }
    assert!(!scratch.at("a.dart").exists());
    assert!(!scratch.at("a.ts").exists());
}

#[test]
fn a_json_document_that_is_not_a_schema_is_a_schema_failure() {
    let scratch = Scratch::new("notaschema");
    let schema = scratch.at("app.json");
    fs::write(&schema, r#"{"hello": "world"}"#).unwrap();
    let request = Generate {
        schema,
        dart: Some(scratch.at("a.dart")),
        ts: None,
        check: false,
    };
    match run(&request) {
        Err(CliError::Schema { source, .. }) => {
            assert!(!source.to_string().contains("not valid JSON"), "{source}");
        }
        other => panic!("expected a schema failure, got {other:?}"),
    }
}

#[test]
fn a_valid_schema_with_no_emittable_spelling_writes_nothing() {
    // A scalar root parses and validates, and has no generated client: the
    // rejection comes from the emitter, after the reader was happy.
    let scratch = Scratch::new("unemittable");
    let schema = scratch.at("app.json");
    fs::write(
        &schema,
        r#"{"root": {"kind": "int"}, "types": [], "version": 1}"#,
    )
    .unwrap();
    let request = Generate {
        schema,
        dart: Some(scratch.at("a.dart")),
        ts: Some(scratch.at("a.ts")),
        check: false,
    };
    match run(&request) {
        Err(CliError::Emit(e)) => assert!(e.to_string().contains("root"), "{e}"),
        other => panic!("expected an emit failure, got {other:?}"),
    }
    assert!(!scratch.at("a.dart").exists());
    assert!(!scratch.at("a.ts").exists());
}

#[test]
fn a_schema_that_is_not_utf8_is_a_read_failure_naming_the_path() {
    let scratch = Scratch::new("notutf8");
    let schema = scratch.at("app.json");
    fs::write(&schema, [0x7b, 0xff, 0xfe, 0x7d]).unwrap();
    let request = Generate {
        schema: schema.clone(),
        dart: Some(scratch.at("a.dart")),
        ts: None,
        check: false,
    };
    match run(&request) {
        Err(CliError::ReadSchema { path, .. }) => assert_eq!(path, schema),
        other => panic!("expected a read failure, got {other:?}"),
    }
}

#[test]
fn an_output_that_cannot_be_read_back_is_a_failure_rather_than_a_difference() {
    // A directory where a file should be. Reporting this as "out of date"
    // would tell the developer to run a command that cannot possibly work.
    let scratch = Scratch::new("unreadable");
    let path = scratch.at("a.duet.dart");
    fs::create_dir_all(&path).unwrap();
    let request = Generate {
        schema: scratch.schema(),
        dart: Some(path.clone()),
        ts: None,
        check: true,
    };
    match run(&request) {
        Err(CliError::ReadOutput { path: at, .. }) => assert_eq!(at, path),
        other => panic!("expected a read failure, got {other:?}"),
    }
}

#[test]
fn asking_for_one_language_generates_only_that_one() {
    let scratch = Scratch::new("onlydart");
    let request = Generate {
        schema: scratch.schema(),
        dart: Some(scratch.at("a.duet.dart")),
        ts: None,
        check: false,
    };
    let report = run(&request).unwrap();
    assert_eq!(report.lines.len(), 1);
    assert!(scratch.at("a.duet.dart").exists());
    assert!(!scratch.at("a.duet.ts").exists());
}

#[test]
fn generating_the_same_schema_twice_gives_byte_identical_files() {
    // Determinism through the CLI, not only through the library: if the header
    // carried a timestamp or the argv order, `--check` would fail at random.
    let scratch = Scratch::new("determinism");
    let request = request(&scratch, false);
    run(&request).unwrap();
    let first = fs::read_to_string(scratch.at("a.duet.ts")).unwrap();
    run(&request).unwrap();
    assert_eq!(first, fs::read_to_string(scratch.at("a.duet.ts")).unwrap());
}

#[test]
fn the_typescript_output_imports_the_published_packages() {
    // What a third party gets. Every committed golden in this repository
    // overrides these specifiers to reach into `src`, so without this the
    // default path would ship untested.
    let scratch = Scratch::new("imports");
    run(&request(&scratch, false)).unwrap();
    let ts = fs::read_to_string(scratch.at("a.duet.ts")).unwrap();
    assert!(ts.contains("from 'duet-protocol'"), "{ts}");
    assert!(ts.contains("from 'duet-protocol/typed'"), "{ts}");
}
