//! The partial-write guarantee, measured rather than argued.
//!
//! Each test here arranges a failure and then asserts on the *other* file —
//! the one that was not the cause. That is the property the module claims, and
//! it is the one a test asserting only "it returned an error" would miss
//! entirely.

use super::*;

/// A scratch directory unique to one test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("duet-cli-write-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the scratch directory should be creatable");
        Scratch(path)
    }

    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leaked temp directory must not fail a passing test.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A target with the given path and content.
fn target(path: PathBuf, content: &str) -> Target {
    Target {
        path,
        content: content.to_string(),
    }
}

/// Makes `path` read-only, or skips the caller if the platform will not.
///
/// Running as root defeats every permission-based test, so this reports whether
/// the restriction actually took rather than assuming it did.
fn make_read_only(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let mut permissions = metadata.permissions();
    permissions.set_readonly(true);
    if fs::set_permissions(path, permissions).is_err() {
        return false;
    }
    // Root ignores the mode bits entirely; prove the restriction bites.
    fs::write(path.join("probe"), "x").is_err()
}

/// Undoes [`make_read_only`] so the directory can be removed.
fn make_writable(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}

#[test]
fn both_files_are_written() {
    let scratch = Scratch::new("both");
    let targets = [
        target(scratch.at("a.dart"), "dart\n"),
        target(scratch.at("b.ts"), "ts\n"),
    ];
    write_all(&targets).expect("a plain write should succeed");
    assert_eq!(fs::read_to_string(scratch.at("a.dart")).unwrap(), "dart\n");
    assert_eq!(fs::read_to_string(scratch.at("b.ts")).unwrap(), "ts\n");
}

#[test]
fn a_missing_parent_directory_is_created() {
    // `duet generate --dart lib/src/generated/app.duet.dart` on a fresh
    // checkout, which is the first thing a new user types.
    let scratch = Scratch::new("mkdir");
    let path = scratch.at("deep/deeper/a.dart");
    write_all(&[target(path.clone(), "x\n")]).expect("the directory should be created");
    assert_eq!(fs::read_to_string(&path).unwrap(), "x\n");
}

#[test]
fn an_existing_file_is_replaced_wholly() {
    let scratch = Scratch::new("replace");
    let path = scratch.at("a.dart");
    fs::write(&path, "a much longer previous version\n").unwrap();
    write_all(&[target(path.clone(), "new\n")]).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
}

#[test]
fn no_temporary_file_survives_a_successful_run() {
    let scratch = Scratch::new("clean");
    write_all(&[
        target(scratch.at("a.dart"), "a\n"),
        target(scratch.at("b.ts"), "b\n"),
    ])
    .unwrap();
    let left: Vec<String> = fs::read_dir(&scratch.0)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("duet-"))
        .collect();
    assert!(left.is_empty(), "temporaries left behind: {left:?}");
}

#[test]
fn a_failure_on_the_second_file_leaves_the_first_untouched() {
    // THE test this module exists for. Without staging-before-renaming, the
    // Dart client here would already be the new one while the TypeScript client
    // stayed old — two clients that both compile and disagree about the store.
    let scratch = Scratch::new("atomic");
    let first = scratch.at("a.dart");
    fs::write(&first, "PREVIOUS\n").unwrap();

    let locked = scratch.at("locked");
    fs::create_dir_all(&locked).unwrap();
    if !make_read_only(&locked) {
        return; // running as root, or a filesystem without permissions
    }

    let error = write_all(&[
        target(first.clone(), "NEW\n"),
        target(locked.join("b.ts"), "NEW\n"),
    ])
    .expect_err("a read-only output directory should fail the run");

    make_writable(&locked);
    assert_eq!(
        fs::read_to_string(&first).unwrap(),
        "PREVIOUS\n",
        "the first file was replaced even though the run failed"
    );
    assert!(
        error.to_string().contains("locked"),
        "the error must name the path: {error}"
    );
}

#[test]
fn a_failed_run_leaves_no_temporary_behind() {
    let scratch = Scratch::new("litter");
    let locked = scratch.at("locked");
    fs::create_dir_all(&locked).unwrap();
    if !make_read_only(&locked) {
        return;
    }
    let _ = write_all(&[
        target(scratch.at("a.dart"), "a\n"),
        target(locked.join("b.ts"), "b\n"),
    ]);
    make_writable(&locked);

    let left: Vec<String> = fs::read_dir(&scratch.0)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("duet-"))
        .collect();
    assert!(left.is_empty(), "temporaries left behind: {left:?}");
}

#[test]
fn an_output_path_that_is_a_directory_fails_without_touching_the_other() {
    // `duet generate --dart lib/` — a natural mistake, and the one failure that
    // staging alone does not catch, because the temporary beside a directory
    // writes perfectly well and only the rename objects. This test failed on
    // the first version of this module, which had already replaced `a.dart` by
    // then; the pre-flight in `stage` is what it bought.
    let scratch = Scratch::new("isdir");
    let first = scratch.at("a.dart");
    fs::write(&first, "PREVIOUS\n").unwrap();
    let directory = scratch.at("b.ts");
    fs::create_dir_all(&directory).unwrap();

    let error = write_all(&[target(first.clone(), "NEW\n"), target(directory, "NEW\n")])
        .expect_err("writing over a directory should fail");
    assert_eq!(
        fs::read_to_string(&first).unwrap(),
        "PREVIOUS\n",
        "the first file was replaced before the second was found to be a directory"
    );
    let message = error.to_string();
    assert!(message.contains("b.ts"), "{message}");
    assert!(message.contains("is a directory"), "{message}");
    assert!(message.contains("name the file to write"), "{message}");
}

#[test]
fn an_empty_target_list_succeeds_and_does_nothing() {
    write_all(&[]).expect("no targets is not a failure");
}

#[test]
fn a_bare_file_name_writes_into_the_current_directory() {
    // `Path::parent` gives `""` here, and `create_dir_all("")` fails, so this
    // is the arm that turns it into `.`.
    let scratch = Scratch::new("bare");
    let previous = std::env::current_dir().expect("a current directory");
    // Not `set_current_dir` — the test harness is multi-threaded and the
    // process-wide cwd is shared. Naming a relative path under a directory that
    // exists reaches the same branch: `parent()` of "x.dart" is empty.
    assert_eq!(
        temporary_path(Path::new("x.dart")),
        PathBuf::from(format!(".x.dart.duet-{}.tmp", std::process::id()))
    );
    let target = target(scratch.at("x.dart"), "x\n");
    write_all(&[target]).unwrap();
    assert_eq!(
        std::env::current_dir().expect("a current directory"),
        previous,
        "no test may change the process-wide working directory"
    );
}

#[test]
fn a_temporary_sits_beside_its_target_so_the_rename_stays_on_one_filesystem() {
    // The reason `rename` is atomic at all. A temporary in the system temp
    // directory would cross a mount on most Linux CI images and fall back to a
    // copy, which is exactly the non-atomic write this module avoids.
    let temporary = temporary_path(Path::new("/some/where/app.duet.dart"));
    assert_eq!(temporary.parent(), Some(Path::new("/some/where")));
    let name = temporary
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with(".app.duet.dart.duet-"), "{name}");
    assert!(name.ends_with(".tmp"), "{name}");
}

#[test]
fn a_write_error_carries_the_path_and_the_cause() {
    let error = WriteError {
        path: PathBuf::from("/nope/a.dart"),
        doing: "write",
        source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
    };
    let message = error.to_string();
    assert!(message.contains("/nope/a.dart"), "{message}");
    assert!(message.contains("denied"), "{message}");
    assert!(std::error::Error::source(&error).is_some());
}
