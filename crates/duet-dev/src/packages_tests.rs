//! Mapping file paths to `package:` URIs.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("duet-dev-pkg-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory should be creatable");
    path
}

/// Writes a `package_config.json` in the shape `flutter pub get` produces —
/// `rootUri` relative to `.dart_tool/`, `packageUri` naming the lib directory.
fn write_config(project: &Path, entries: &[(&str, &str, &str)]) -> PathBuf {
    let tool = project.join(".dart_tool");
    std::fs::create_dir_all(&tool).expect("mkdir");
    let packages: Vec<String> = entries
        .iter()
        .map(|(name, root, lib)| {
            format!(r#"{{"name":"{name}","rootUri":"{root}","packageUri":"{lib}"}}"#)
        })
        .collect();
    let path = tool.join("package_config.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"configVersion":2,"packages":[{}]}}"#,
            packages.join(",")
        ),
    )
    .expect("write");
    path
}

#[test]
fn a_file_under_the_projects_lib_becomes_a_package_uri() {
    // The headline case, and the reason this module exists: `flutter build`
    // compiled the running program with `package:` URIs, so the incremental
    // diff has to identify the same library the same way or the VM treats it
    // as a new one and the edit silently does not take effect.
    let project = scratch("solo");
    let config = write_config(&project, &[("duet_guest", "../", "lib/")]);
    let packages = PackageConfig::read(&config).expect("the config should read");

    assert_eq!(packages.len(), 1);
    assert!(!packages.is_empty());
    assert_eq!(
        packages.uri_for(&project.join("lib/main.dart")),
        "package:duet_guest/main.dart"
    );
    assert_eq!(
        packages.uri_for(&project.join("lib/src/deep/thing.dart")),
        "package:duet_guest/src/deep/thing.dart",
        "nested files keep their relative path, with forward slashes"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_file_outside_every_package_falls_back_to_a_file_uri() {
    // `frontend_server` accepts both, so a file the config does not cover
    // still compiles — worse identity, but not a failure.
    let project = scratch("outside");
    let config = write_config(&project, &[("duet_guest", "../", "lib/")]);
    let packages = PackageConfig::read(&config).expect("read");
    let stray = project.join("tool/generate.dart");
    assert_eq!(
        packages.uri_for(&stray),
        format!("file://{}", stray.display())
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn path_dependencies_resolve_to_their_own_package() {
    // A Duet project has `duet` and `duet_flutter` as path dependencies, so
    // this is the normal case, not an exotic one. Editing a file in one of
    // those must invalidate it under *its* package name.
    let workspace = scratch("workspace");
    let app = workspace.join("app");
    std::fs::create_dir_all(&app).expect("mkdir");
    let config = write_config(
        &app,
        &[
            ("duet_guest", "../", "lib/"),
            ("duet", "../../packages/duet", "lib/"),
        ],
    );
    let packages = PackageConfig::read(&config).expect("read");

    assert_eq!(
        packages.uri_for(&app.join("lib/main.dart")),
        "package:duet_guest/main.dart"
    );
    assert_eq!(
        packages.uri_for(&workspace.join("packages/duet/lib/duet.dart")),
        "package:duet/duet.dart",
        "a path dependency resolves under its own name"
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn the_longest_matching_package_wins() {
    // A package nested inside another package's tree — an example app inside
    // a plugin repository, which is a standard layout. The inner one must win,
    // or every file in it gets the outer package's name.
    let root = scratch("nested");
    let config = write_config(
        &root,
        &[
            ("outer", "../", "lib/"),
            ("inner", "../lib/vendored", "src/"),
        ],
    );
    let packages = PackageConfig::read(&config).expect("read");
    assert_eq!(
        packages.uri_for(&root.join("lib/vendored/src/thing.dart")),
        "package:inner/thing.dart",
        "the deeper mapping should win"
    );
    assert_eq!(
        packages.uri_for(&root.join("lib/normal.dart")),
        "package:outer/normal.dart"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_file_scheme_root_uri_is_understood() {
    // `pub get` writes relative roots, but a `file://` one is legal and some
    // tools produce it.
    let project = scratch("fileuri");
    let lib = project.join("lib");
    std::fs::create_dir_all(&lib).expect("mkdir");
    let config = write_config(
        &project,
        &[("absolute", &format!("file://{}", project.display()), "lib/")],
    );
    let packages = PackageConfig::read(&config).expect("read");
    assert_eq!(
        packages.uri_for(&lib.join("x.dart")),
        "package:absolute/x.dart"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_malformed_entry_is_skipped_rather_than_failing_the_whole_read() {
    // One odd entry must not stop a reload session; the consequence of
    // skipping it is a `file://` fallback for that package, which still
    // compiles.
    let project = scratch("malformed");
    let tool = project.join(".dart_tool");
    std::fs::create_dir_all(&tool).expect("mkdir");
    let config = tool.join("package_config.json");
    std::fs::write(
        &config,
        r#"{"packages":[
            {"rootUri":"../","packageUri":"lib/"},
            {"name":"nameless"},
            {"name":"good","rootUri":"../","packageUri":"lib/"},
            "not even an object"
        ]}"#,
    )
    .expect("write");

    let packages = PackageConfig::read(&config).expect("a mixed config should still read");
    assert_eq!(packages.len(), 1, "only the well-formed entry is kept");
    assert_eq!(
        packages.uri_for(&project.join("lib/a.dart")),
        "package:good/a.dart"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_config_with_no_packages_array_yields_an_empty_map() {
    let project = scratch("empty");
    let tool = project.join(".dart_tool");
    std::fs::create_dir_all(&tool).expect("mkdir");
    let config = tool.join("package_config.json");
    std::fs::write(&config, r#"{"configVersion":2}"#).expect("write");
    let packages = PackageConfig::read(&config).expect("read");
    assert!(packages.is_empty());
    assert_eq!(packages.len(), 0);
    // Everything falls back, which still compiles.
    assert!(
        packages
            .uri_for(Path::new("/x/y.dart"))
            .starts_with("file://")
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn an_unreadable_or_invalid_config_is_an_error_that_names_the_file() {
    let Err(missing) = PackageConfig::read("/definitely/not/here/package_config.json") else {
        panic!("a missing config should fail");
    };
    assert_eq!(missing.stage(), Stage::LocateSdk);

    let project = scratch("invalid");
    let config = project.join("package_config.json");
    std::fs::write(&config, "this is not json").expect("write");
    let Err(bad) = PackageConfig::read(&config) else {
        panic!("invalid JSON should fail");
    };
    assert!(
        bad.to_string().contains("valid JSON"),
        "the message should say what is wrong: {bad}"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn the_default_config_maps_nothing_and_is_harmless() {
    // `PackageConfig::default()` is what a caller gets before reading one.
    let packages = PackageConfig::default();
    assert!(packages.is_empty());
    assert!(
        packages
            .uri_for(Path::new("/a/b.dart"))
            .starts_with("file://")
    );
}
