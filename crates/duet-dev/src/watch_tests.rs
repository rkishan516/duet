//! The watcher: detection, debounce, and what it refuses to look at.
//!
//! The clock is injected, so the debounce is asserted rather than slept
//! through — a test that sleeps for its debounce is a test that is flaky on a
//! loaded CI runner.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("duet-dev-watch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory should be creatable");
    path
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, contents).expect("write");
}

fn config(root: &Path, debounce: Duration) -> WatchConfig {
    WatchConfig {
        roots: vec![root.to_path_buf()],
        extensions: vec!["dart".to_string()],
        debounce,
    }
}

/// A watcher over `root` with an effectively instant debounce, for the tests
/// about *detection* rather than about timing.
fn instant(root: &Path) -> Watcher {
    Watcher::new(config(root, Duration::ZERO)).expect("the root exists")
}

#[test]
fn the_baseline_scan_means_the_first_poll_reports_nothing() {
    // Otherwise `duet dev` would do a full recompile the instant it started,
    // every time, for no reason.
    let root = scratch("baseline");
    write(&root.join("main.dart"), "void main() {}");
    let mut watcher = instant(&root);
    assert_eq!(watcher.watched(), 1);
    assert_eq!(
        watcher.poll(Instant::now()),
        None,
        "files that already existed are known, not changed"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_modified_file_is_reported() {
    let root = scratch("modify");
    let file = root.join("main.dart");
    write(&file, "void main() {}");
    let mut watcher = instant(&root);

    write(&file, "void main() { print('changed'); }");
    let batch = watcher
        .poll(Instant::now())
        .expect("a modified file should be reported");
    assert_eq!(batch, vec![file.clone()]);

    assert_eq!(
        watcher.poll(Instant::now()),
        None,
        "the same change must not be reported twice"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rewrite_of_the_same_length_within_the_same_instant_is_still_caught() {
    // The case size-only comparison misses and mtime-only comparison can miss
    // on a coarse filesystem. Both stamps together are why this passes.
    let root = scratch("samelen");
    let file = root.join("main.dart");
    write(&file, "const String kMarker = 'MARKER_V1';");
    let mut watcher = instant(&root);
    write(&file, "const String kMarker = 'MARKER_V2';");
    assert_eq!(
        watcher.poll(Instant::now()),
        Some(vec![file]),
        "a same-length edit is still an edit"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn creations_and_deletions_are_both_changes() {
    // A deletion invalidates the file just as a modification does — the
    // compiler needs to know it is gone.
    let root = scratch("createdelete");
    let existing = root.join("a.dart");
    write(&existing, "// a");
    let mut watcher = instant(&root);

    let created = root.join("b.dart");
    write(&created, "// b");
    assert_eq!(watcher.poll(Instant::now()), Some(vec![created.clone()]));

    std::fs::remove_file(&created).expect("rm");
    assert_eq!(
        watcher.poll(Instant::now()),
        Some(vec![created]),
        "a deletion is a change"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_rename_is_reported_as_both_paths() {
    let root = scratch("rename");
    let from = root.join("old.dart");
    let to = root.join("new.dart");
    write(&from, "// x");
    let mut watcher = instant(&root);
    std::fs::rename(&from, &to).expect("rename");

    let mut batch = watcher.poll(Instant::now()).expect("a rename is a change");
    batch.sort();
    let mut expected = vec![from, to];
    expected.sort();
    assert_eq!(batch, expected);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nested_directories_are_walked() {
    let root = scratch("nested");
    let deep = root.join("src/widgets/deep.dart");
    write(&deep, "// deep");
    let mut watcher = instant(&root);
    assert_eq!(watcher.watched(), 1);
    write(&deep, "// deeper");
    assert_eq!(watcher.poll(Instant::now()), Some(vec![deep]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_the_configured_extensions_count() {
    // Recompiling because a README changed would be pure latency.
    let root = scratch("extensions");
    write(&root.join("main.dart"), "// dart");
    write(&root.join("README.md"), "# hi");
    write(&root.join("noextension"), "x");
    let mut watcher = instant(&root);
    assert_eq!(watcher.watched(), 1, "only the .dart file is watched");

    write(&root.join("README.md"), "# changed");
    write(&root.join("noextension"), "y");
    assert_eq!(
        watcher.poll(Instant::now()),
        None,
        "unwatched extensions are not changes"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn extension_matching_is_case_insensitive() {
    // macOS filesystems are case-insensitive by default, so `Main.Dart` and
    // `main.dart` are the same file; treating the extension as case-sensitive
    // would watch one spelling and not the other.
    let root = scratch("case");
    write(&root.join("Main.DART"), "// x");
    let watcher = instant(&root);
    assert_eq!(watcher.watched(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn generated_output_directories_are_never_descended_into() {
    // The critical one: this crate writes its incremental dill under a build
    // directory, and watching it would make every reload trigger the next —
    // an infinite loop that looks like the tool has gone mad.
    let root = scratch("skipped");
    write(&root.join("main.dart"), "// real");
    for skipped in [".dart_tool", "build", ".git", ".idea", "ios", ".hidden"] {
        write(&root.join(skipped).join("generated.dart"), "// noise");
    }
    let mut watcher = instant(&root);
    assert_eq!(
        watcher.watched(),
        1,
        "only the real source file should be watched"
    );

    write(&root.join("build/generated.dart"), "// changed noise");
    write(&root.join(".dart_tool/generated.dart"), "// changed noise");
    assert_eq!(
        watcher.poll(Instant::now()),
        None,
        "generated output must never trigger a reload"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn changes_are_held_until_the_tree_has_been_quiet() {
    // Format-on-save writes twice in quick succession. Recompiling on the
    // first write means compiling a file the editor is still rewriting.
    let root = scratch("debounce");
    let file = root.join("main.dart");
    write(&file, "// v1");
    let debounce = Duration::from_millis(120);
    let mut watcher = Watcher::new(config(&root, debounce)).expect("root exists");

    let t0 = Instant::now();
    write(&file, "// v2");
    assert_eq!(watcher.poll(t0), None, "still settling");
    assert_eq!(
        watcher.poll(t0 + Duration::from_millis(119)),
        None,
        "one millisecond short of the quiet period"
    );
    assert_eq!(
        watcher.poll(t0 + debounce),
        Some(vec![file]),
        "released once the tree has been quiet for the full period"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_further_change_during_the_quiet_period_extends_it() {
    // The trailing-edge property. A save storm should produce one batch at the
    // end, not one batch per write.
    let root = scratch("extend");
    let first = root.join("a.dart");
    let second = root.join("b.dart");
    write(&first, "// a1");
    write(&second, "// b1");
    let debounce = Duration::from_millis(100);
    let mut watcher = Watcher::new(config(&root, debounce)).expect("root exists");

    let t0 = Instant::now();
    write(&first, "// a2");
    assert_eq!(watcher.poll(t0), None);

    // A second change 80 ms in resets the clock.
    let t1 = t0 + Duration::from_millis(80);
    write(&second, "// b2");
    assert_eq!(watcher.poll(t1), None, "the second change extends the wait");
    assert_eq!(
        watcher.poll(t1 + Duration::from_millis(99)),
        None,
        "still inside the extended period"
    );

    let mut batch = watcher
        .poll(t1 + debounce)
        .expect("the batch should be released eventually");
    batch.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(
        batch, expected,
        "the batch is the union of everything that changed"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_file_changed_twice_appears_once_in_the_batch() {
    // The batch becomes the compiler's invalidated-file list; a duplicate
    // there is harmless but sloppy, and a `recompile` listing the same URI
    // twice is not something the protocol promises to like.
    let root = scratch("dedupe");
    let file = root.join("main.dart");
    write(&file, "// 1");
    let debounce = Duration::from_millis(50);
    let mut watcher = Watcher::new(config(&root, debounce)).expect("root exists");

    let t0 = Instant::now();
    write(&file, "// 2");
    assert_eq!(watcher.poll(t0), None);
    write(&file, "// 3 longer");
    assert_eq!(watcher.poll(t0 + Duration::from_millis(10)), None);
    assert_eq!(
        watcher.poll(t0 + Duration::from_millis(60)),
        Some(vec![file]),
        "one entry, not two"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn polling_a_quiet_tree_forever_produces_nothing() {
    // The steady state of a dev session. A watcher that eventually emitted a
    // spurious batch would recompile at random.
    let root = scratch("quiet");
    write(&root.join("main.dart"), "// x");
    let mut watcher = Watcher::new(config(&root, Duration::from_millis(10))).expect("root exists");
    let t0 = Instant::now();
    for tick in 0..50 {
        assert_eq!(
            watcher.poll(t0 + Duration::from_millis(tick * 20)),
            None,
            "a quiet tree produces no batches"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_root_fails_at_construction_rather_than_watching_nothing() {
    // A mistyped project path should stop `duet dev` at startup, not leave it
    // apparently running and silently never reloading.
    let Err(e) = Watcher::new(WatchConfig {
        roots: vec![PathBuf::from("/definitely/not/a/directory")],
        extensions: vec!["dart".to_string()],
        debounce: Duration::ZERO,
    }) else {
        panic!("a missing root should fail");
    };
    assert_eq!(e.stage(), Stage::Watch);
    assert!(
        e.to_string().contains("/definitely/not/a/directory"),
        "got {e}"
    );
}

#[test]
fn a_root_that_disappears_mid_session_does_not_panic() {
    // `read_dir` failing must not take the session down; the files vanishing
    // is already reported as changes, which is exactly right.
    let root = scratch("vanishing");
    write(&root.join("main.dart"), "// x");
    let mut watcher = instant(&root);
    std::fs::remove_dir_all(&root).expect("rmdir");
    assert_eq!(
        watcher.poll(Instant::now()),
        Some(vec![root.join("main.dart")]),
        "the file is gone, which is a change"
    );
    assert_eq!(watcher.poll(Instant::now()), None, "and then nothing");
}

#[test]
fn the_dart_project_preset_watches_lib_for_dart_files() {
    // The shape `duet dev` uses. Watching the project root instead of `lib/`
    // would pull in `.dart_tool` before the skip list even applies.
    let config = WatchConfig::dart_project("/some/project");
    assert_eq!(config.roots, vec![PathBuf::from("/some/project/lib")]);
    assert_eq!(config.extensions, vec!["dart".to_string()]);
    assert!(
        config.debounce >= Duration::from_millis(50)
            && config.debounce <= Duration::from_millis(300),
        "the debounce should be long enough for format-on-save and short \
         enough to stay invisible, got {:?}",
        config.debounce
    );
}
