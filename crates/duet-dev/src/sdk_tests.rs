//! SDK resolution, and the error a wrong path produces.

use super::*;

/// A unique scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("duet-dev-sdk-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory should be creatable");
    path
}

/// Builds the directory layout `FlutterSdk::locate` expects. The runtime file
/// carries the platform's name ([`DART_AOT_RUNTIME`]), exactly as a real SDK
/// checkout does — `dartaotruntime.exe` on Windows, bare elsewhere.
fn fake_flutter_root(root: &Path) {
    let dart_bin = root.join("bin/cache/dart-sdk/bin");
    std::fs::create_dir_all(dart_bin.join("snapshots")).expect("mkdir");
    std::fs::write(dart_bin.join(DART_AOT_RUNTIME), "").expect("write");
    std::fs::write(
        dart_bin.join("snapshots/frontend_server_aot.dart.snapshot"),
        "",
    )
    .expect("write");
    std::fs::create_dir_all(root.join("bin/cache/artifacts/engine/common/flutter_patched_sdk"))
        .expect("mkdir");
}

#[test]
fn a_complete_flutter_root_resolves_all_three_artefacts() {
    // The layout asserted here is the one Spike C recorded working against the
    // real SDK, so a rename in a future Flutter would fail this rather than
    // producing a mysterious compiler hang.
    let root = scratch("complete");
    fake_flutter_root(&root);
    let sdk = FlutterSdk::locate(&root).expect("a complete root should resolve");

    assert!(
        sdk.dartaotruntime
            .ends_with(Path::new("bin/cache/dart-sdk/bin").join(DART_AOT_RUNTIME))
    );
    assert!(
        sdk.frontend_server
            .ends_with("snapshots/frontend_server_aot.dart.snapshot")
    );
    assert!(sdk.patched_sdk.ends_with("common/flutter_patched_sdk"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_sdk_root_argument_always_ends_with_a_separator() {
    // Not cosmetic: Spike C recorded that `frontend_server` "silently
    // misbehaves" without it, resolving `dart:core` against a path that does
    // not exist and failing much later with something unrelated-looking.
    let root = scratch("trailing");
    fake_flutter_root(&root);
    let sdk = FlutterSdk::locate(&root).expect("should resolve");
    let argument = sdk.sdk_root_argument();
    assert!(
        argument.ends_with(std::path::MAIN_SEPARATOR),
        "{argument} must end with a separator"
    );
    // Idempotent — appending twice would produce `//`, which is harmless but
    // suggests the check is missing.
    assert!(
        !argument.ends_with("//"),
        "{argument} has a doubled separator"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_root_is_reported_once_rather_than_three_times() {
    // A typo in the Flutter path would otherwise produce a confusing cascade;
    // naming the root itself is the useful message.
    let Err(e) = FlutterSdk::locate("/definitely/not/a/flutter/checkout") else {
        panic!("a missing root should not resolve");
    };
    assert_eq!(e.stage(), Stage::LocateSdk);
    let text = e.to_string();
    assert!(
        text.contains("Flutter SDK directory"),
        "the root itself is what is missing: {text}"
    );
}

#[test]
fn each_missing_artefact_is_named_individually() {
    // A half-populated cache is a real state — an interrupted `flutter
    // precache` leaves exactly this — and the message has to say which piece
    // is absent.
    let aot_runtime = format!("bin/cache/dart-sdk/bin/{DART_AOT_RUNTIME}");
    let cases: [(&str, &str); 3] = [
        (aot_runtime.as_str(), "dartaotruntime"),
        (
            "bin/cache/dart-sdk/bin/snapshots/frontend_server_aot.dart.snapshot",
            "frontend_server snapshot",
        ),
        (
            "bin/cache/artifacts/engine/common/flutter_patched_sdk",
            "flutter_patched_sdk directory",
        ),
    ];
    for (index, (remove, expected)) in cases.into_iter().enumerate() {
        let root = scratch(&format!("partial{index}"));
        fake_flutter_root(&root);
        // Rebuilt from components so its display uses the platform's own
        // separators — the error message being checked against was built with
        // `.join()` chains, and a mixed-separator expectation can never match
        // it on Windows.
        let victim: PathBuf = root.join(remove).components().collect();
        if victim.is_dir() {
            std::fs::remove_dir_all(&victim).expect("rmdir");
        } else {
            std::fs::remove_file(&victim).expect("rm");
        }

        let Err(e) = FlutterSdk::locate(&root) else {
            panic!("removing {remove} should make resolution fail");
        };
        let text = e.to_string();
        assert!(
            text.contains(expected),
            "removing {remove} should be reported as {expected:?}, got {text}"
        );
        assert!(
            text.contains(&victim.display().to_string()),
            "the message should name the path it looked at: {text}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn a_package_config_is_found_under_a_project() {
    let project = scratch("project");
    std::fs::create_dir_all(project.join(".dart_tool")).expect("mkdir");
    std::fs::write(project.join(".dart_tool/package_config.json"), "{}").expect("write");
    let found = package_config(&project).expect("it should be found");
    assert!(found.ends_with(".dart_tool/package_config.json"));
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_project_that_has_not_had_pub_get_run_says_so() {
    // The distinct failure this is split from `FlutterSdk` for: the fix is
    // `flutter pub get`, not installing Flutter.
    let project = scratch("nopubget");
    let Err(e) = package_config(&project) else {
        panic!("a project with no .dart_tool should fail");
    };
    assert!(
        e.to_string().contains("pub get"),
        "the message should name the fix: {e}"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_missing_project_directory_is_reported_as_such() {
    let Err(e) = package_config("/definitely/not/a/project") else {
        panic!("a missing project should fail");
    };
    assert!(
        e.to_string().contains("Flutter project directory"),
        "got {e}"
    );
}
