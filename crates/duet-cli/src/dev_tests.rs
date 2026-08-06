//! `duet dev`'s startup failures, and the decisions that do not need an SDK.
//!
//! A whole session needs a Flutter SDK, a compiled Flutter project and a real
//! engine, none of which CI has — that proof lives in
//! `crates/duet-backend-macos/examples/hot_reload.rs`, which boots a real one.
//! What is reachable here is everything a developer hits *before* that works:
//! a wrong path, a missing `FLUTTER_ROOT`, a host that never announces a VM
//! service. Those are the paths that decide whether the tool is usable when it
//! fails, and they have no other cover.

use super::*;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("duet-cli-dev-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory should be creatable");
    path
}

fn request(project: &Path, flutter_root: Option<&Path>, host: &[&str]) -> Dev {
    Dev {
        project: project.to_path_buf(),
        flutter_root: flutter_root.map(Path::to_path_buf),
        entrypoint: None,
        host: host.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// Runs a request, returning `(code, stdout, stderr)`.
fn drive(request: &Dev) -> (u8, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(request, &mut out, &mut err);
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

#[test]
fn the_flag_wins_over_the_environment() {
    let chosen = resolve_flutter_root(Some(Path::new("/from/flag")), Some("/from/env"))
        .expect("an explicit root is always usable");
    assert_eq!(chosen, PathBuf::from("/from/flag"));
}

#[test]
fn the_environment_is_used_when_the_flag_is_absent() {
    // `flutter` itself sets `FLUTTER_ROOT`, so a developer running under it
    // should not have to repeat the path.
    let chosen =
        resolve_flutter_root(None, Some("/from/env")).expect("the environment should be used");
    assert_eq!(chosen, PathBuf::from("/from/env"));
}

#[test]
fn no_flutter_root_anywhere_names_both_ways_of_giving_one() {
    // A developer hitting this has no way to know the environment variable is
    // consulted at all unless the message says so.
    for environment in [None, Some(""), Some("   ")] {
        let Err(e) = resolve_flutter_root(None, environment) else {
            panic!("{environment:?} should not resolve to an SDK");
        };
        let text = e.to_string();
        assert!(text.contains("--flutter-root"), "got {text}");
        assert!(text.contains("FLUTTER_ROOT"), "got {text}");
    }
}

#[test]
fn a_missing_flutter_sdk_fails_fast_and_says_what_to_try() {
    // Before anything is spawned: a wrong `--flutter-root` should cost a
    // second, not a full `cargo run` and then a timeout.
    let project = scratch("nosdk");
    let (code, _, err) = drive(&request(
        &project,
        Some(Path::new("/definitely/not/flutter")),
        &["true"],
    ));
    assert_eq!(code, crate::EXIT_FAILED);
    assert!(
        err.contains("[locate-sdk]"),
        "the stage belongs in the message: {err}"
    );
    assert!(
        err.contains("precache") || err.contains("--flutter-root"),
        "the advice should name a next step: {err}"
    );
    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn a_project_without_pub_get_says_so_rather_than_failing_obscurely() {
    // The second most likely first-run failure, and the one whose fix is least
    // guessable from a generic error.
    let root = scratch("nopubget");
    let flutter = root.join("flutter");
    let dart_bin = flutter.join("bin/cache/dart-sdk/bin");
    std::fs::create_dir_all(dart_bin.join("snapshots")).expect("mkdir");
    std::fs::write(dart_bin.join("dartaotruntime"), "").expect("write");
    std::fs::write(
        dart_bin.join("snapshots/frontend_server_aot.dart.snapshot"),
        "",
    )
    .expect("write");
    std::fs::create_dir_all(flutter.join("bin/cache/artifacts/engine/common/flutter_patched_sdk"))
        .expect("mkdir");

    let project = root.join("app");
    std::fs::create_dir_all(&project).expect("mkdir");

    let (code, _, err) = drive(&request(&project, Some(&flutter), &["true"]));
    assert_eq!(code, crate::EXIT_FAILED);
    assert!(
        err.contains("pub get"),
        "the message should name the fix: {err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_host_that_exits_without_announcing_a_vm_service_says_why() {
    // The failure a release/AOT host produces, and one where a bare timeout
    // would be actively misleading — nothing is hung, the host simply has no
    // VM service to announce.
    let root = scratch("noannounce");
    let flutter = root.join("flutter");
    let dart_bin = flutter.join("bin/cache/dart-sdk/bin");
    std::fs::create_dir_all(dart_bin.join("snapshots")).expect("mkdir");
    std::fs::write(dart_bin.join("dartaotruntime"), "").expect("write");
    std::fs::write(
        dart_bin.join("snapshots/frontend_server_aot.dart.snapshot"),
        "",
    )
    .expect("write");
    std::fs::create_dir_all(flutter.join("bin/cache/artifacts/engine/common/flutter_patched_sdk"))
        .expect("mkdir");

    let project = root.join("app");
    std::fs::create_dir_all(project.join(".dart_tool")).expect("mkdir");
    std::fs::create_dir_all(project.join("lib")).expect("mkdir");
    std::fs::write(
        project.join(".dart_tool/package_config.json"),
        r#"{"configVersion":2,"packages":[{"name":"app","rootUri":"../","packageUri":"lib/"}]}"#,
    )
    .expect("write");
    std::fs::write(project.join("lib/main.dart"), "void main() {}").expect("write");

    // `true` exits immediately, printing nothing — exactly what a host with no
    // VM service looks like.
    let (code, out, err) = drive(&request(&project, Some(&flutter), &["true"]));
    assert_eq!(code, crate::EXIT_FAILED);
    assert!(
        out.contains("duet dev"),
        "the startup banner should have printed first: {out}"
    );
    assert!(
        out.contains("watching"),
        "the banner should say what is being watched: {out}"
    );
    assert!(
        err.contains("[locate-vm-service]"),
        "the stage belongs in the message: {err}"
    );
    assert!(
        err.contains("debug") || err.contains("JIT"),
        "the advice should mention the debug/JIT requirement: {err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_host_command_that_cannot_be_started_is_reported_with_the_command() {
    let root = scratch("nohost");
    let flutter = root.join("flutter");
    let dart_bin = flutter.join("bin/cache/dart-sdk/bin");
    std::fs::create_dir_all(dart_bin.join("snapshots")).expect("mkdir");
    std::fs::write(dart_bin.join("dartaotruntime"), "").expect("write");
    std::fs::write(
        dart_bin.join("snapshots/frontend_server_aot.dart.snapshot"),
        "",
    )
    .expect("write");
    std::fs::create_dir_all(flutter.join("bin/cache/artifacts/engine/common/flutter_patched_sdk"))
        .expect("mkdir");

    let project = root.join("app");
    std::fs::create_dir_all(project.join(".dart_tool")).expect("mkdir");
    std::fs::create_dir_all(project.join("lib")).expect("mkdir");
    std::fs::write(
        project.join(".dart_tool/package_config.json"),
        r#"{"configVersion":2,"packages":[]}"#,
    )
    .expect("write");

    let (code, _, err) = drive(&request(
        &project,
        Some(&flutter),
        &["definitely-not-a-program-on-this-machine"],
    ));
    assert_eq!(code, crate::EXIT_FAILED);
    assert!(
        err.contains("starting the host process"),
        "the message should say what it was doing: {err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_work_directory_is_inside_dart_tool_so_it_cannot_trigger_its_own_reload() {
    // The incremental compiler writes kernel on every reload. If that landed
    // anywhere the watcher looks, every reload would trigger the next one
    // forever. `.dart_tool/` is both generated output and one of the
    // directories the watcher never descends into.
    let work = work_dir(Path::new("/some/project"));
    assert!(
        work.starts_with("/some/project/.dart_tool"),
        "the compiler's output must live under .dart_tool, got {}",
        work.display()
    );
    let watched = duet_dev::WatchConfig::dart_project("/some/project");
    for root in &watched.roots {
        assert!(
            !work.starts_with(root),
            "the compiler writes into {}, which is watched",
            work.display()
        );
    }
}

#[test]
fn every_stage_has_advice_that_names_a_next_step() {
    // The advice line is the difference between an error a developer can act
    // on and one they file a bug about. A stage with an empty or generic line
    // is the same as no advice at all.
    for stage in [
        Stage::LocateSdk,
        Stage::SpawnCompiler,
        Stage::BaselineCompile,
        Stage::LocateVmService,
        Stage::Connect,
        Stage::FindIsolate,
        Stage::Recompile,
        Stage::ReloadSources,
        Stage::Reassemble,
        Stage::Watch,
    ] {
        let text = advice(&DevError::Timeout {
            stage,
            after: Duration::from_secs(1),
        });
        assert!(
            text.len() > 40,
            "the advice for {stage} says too little: {text:?}"
        );
    }
}

// ===================== the pieces a session is built from =====================
//
// Each of these takes its input as an argument rather than reaching for a
// process or a socket, which is what makes the session's behaviour reachable
// without a Flutter SDK. What is left uncovered after this is the loop itself,
// which genuinely needs a booted engine — and that is what
// `crates/duet-backend-macos/examples/hot_reload.rs` drives.

/// A channel pre-loaded with `lines`, as the host's stdout reader would fill
/// it.
fn feed(lines: &[&str]) -> std::sync::mpsc::Receiver<String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    for line in lines {
        sender
            .send((*line).to_string())
            .expect("the receiver is alive");
    }
    receiver
}

#[test]
fn the_announcement_is_found_and_every_line_before_it_is_passed_through() {
    // Echoing is not a nicety: this process owns the pipe, so a line it
    // swallowed is one the developer never sees — and during startup those are
    // exactly the lines that explain why the host is slow.
    let lines = feed(&[
        "   Compiling my-host v0.1.0",
        "warning: unused variable",
        "flutter: The Dart VM service is listening on http://127.0.0.1:56050/abc=/",
    ]);
    let mut out = Vec::new();
    let url = await_announcement(&lines, &mut out, Duration::from_secs(5))
        .expect("the announcement should be found");
    assert_eq!(url.websocket(), "ws://127.0.0.1:56050/abc=/ws");

    let echoed = String::from_utf8_lossy(&out).into_owned();
    assert!(echoed.contains("Compiling my-host"), "got {echoed}");
    assert!(echoed.contains("unused variable"), "got {echoed}");
    assert!(
        echoed.contains("The Dart VM service"),
        "the announcement line is passed through too: {echoed}"
    );
}

#[test]
fn a_host_that_says_nothing_useful_times_out_at_the_locate_stage() {
    // The channel stays open — the host is alive and quiet, which is a hang
    // rather than an exit, and must be reported as one.
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut out = Vec::new();
    let Err(e) = await_announcement(&receiver, &mut out, Duration::from_millis(150)) else {
        panic!("a silent host should not produce a URL");
    };
    assert!(
        matches!(
            e,
            DevError::Timeout {
                stage: Stage::LocateVmService,
                ..
            }
        ),
        "got {e:?}"
    );
    drop(sender);
}

#[test]
fn a_host_that_exits_without_announcing_is_distinguished_from_one_that_hangs() {
    // A closed channel means the host's stdout hit EOF: it exited. Reporting
    // that as a timeout would send a developer looking for a wedged process
    // that is not running at all.
    let lines = feed(&["some output", "then it stopped"]);
    let mut out = Vec::new();
    let Err(e) = await_announcement(&lines, &mut out, Duration::from_secs(30)) else {
        panic!("a host that exited should not produce a URL");
    };
    assert!(
        matches!(e, DevError::NotFound { .. }),
        "an exit is not a timeout: {e:?}"
    );
    assert!(
        e.to_string().contains("JIT"),
        "the message should name the likeliest cause: {e}"
    );
}

#[test]
fn draining_passes_the_hosts_output_through_without_blocking() {
    let lines = feed(&["one", "two"]);
    let mut out = Vec::new();
    drain(&lines, &mut out);
    assert_eq!(String::from_utf8_lossy(&out), "one\ntwo\n");
    // A second drain with nothing waiting must return rather than block.
    drain(&lines, &mut out);
    assert_eq!(String::from_utf8_lossy(&out), "one\ntwo\n");
}

#[test]
fn a_successful_reload_reports_its_timings_and_library_counts() {
    // The counts are what distinguish an incremental reload from a full one,
    // and a developer watching this line is the only person who will notice if
    // that ever changes.
    let mut out = Vec::new();
    let mut err = Vec::new();
    report_reload(
        &Reload::Applied {
            report: duet_dev::ReloadReport {
                success: true,
                received_libraries: Some(2),
                saved_libraries: Some(752),
                notices: Vec::new(),
            },
            timings: duet_dev::Timings {
                recompile: Duration::from_millis(11),
                reload: Duration::from_millis(40),
                reassemble: Duration::from_millis(3),
                total: Duration::from_millis(58),
            },
        },
        &mut out,
        &mut err,
    );
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(text.contains("reloaded in 58 ms"), "got {text}");
    assert!(text.contains("recompile 11 ms"), "got {text}");
    assert!(text.contains("752 kept"), "got {text}");
    assert!(err.is_empty(), "a success writes nothing to stderr");
}

#[test]
fn a_successful_reload_without_counts_still_reports_one_clean_line() {
    // An SDK that stops sending `details` must not produce a line with a
    // dangling fragment in it.
    let mut out = Vec::new();
    let mut err = Vec::new();
    report_reload(
        &Reload::Applied {
            report: duet_dev::ReloadReport {
                success: true,
                received_libraries: None,
                saved_libraries: None,
                notices: Vec::new(),
            },
            timings: duet_dev::Timings::default(),
        },
        &mut out,
        &mut err,
    );
    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(text.contains("reloaded in 0 ms"), "got {text}");
    assert!(
        !text.contains("kept"),
        "no counts means no counts clause: {text}"
    );
}

#[test]
fn a_declined_reload_says_a_restart_is_needed_and_repeats_the_vms_reason() {
    // The developer's next action is different from every other outcome here:
    // not "fix the code", but "restart the host".
    let mut out = Vec::new();
    let mut err = Vec::new();
    report_reload(
        &Reload::Declined {
            report: duet_dev::ReloadReport {
                success: false,
                received_libraries: None,
                saved_libraries: None,
                notices: vec!["Const class cannot remove fields".to_string()],
            },
            timings: duet_dev::Timings::default(),
        },
        &mut out,
        &mut err,
    );
    let text = String::from_utf8_lossy(&err).into_owned();
    assert!(
        text.contains("Const class cannot remove fields"),
        "got {text}"
    );
    assert!(text.contains("Restart"), "got {text}");
    assert!(out.is_empty(), "a decline is a diagnostic, not progress");
}

#[test]
fn a_compile_error_prints_the_compilers_own_diagnostics_and_says_the_host_lives() {
    // The most common outcome in a dev loop. Two things matter: the real
    // diagnostics, untruncated, and the reassurance that nothing was lost —
    // otherwise a developer restarts out of caution and throws away the state.
    let mut out = Vec::new();
    let mut err = Vec::new();
    report_reload(
        &Reload::CompileFailed {
            diagnostics: vec![
                "lib/main.dart:4:12: Error: Expected ';' after this.".to_string(),
                "lib/main.dart:9:1: Error: Undefined name 'oops'.".to_string(),
            ],
            elapsed: Duration::from_millis(14),
        },
        &mut out,
        &mut err,
    );
    let text = String::from_utf8_lossy(&err).into_owned();
    assert!(text.contains("Expected ';'"), "got {text}");
    assert!(text.contains("Undefined name"), "got {text}");
    assert!(text.contains("14 ms"), "got {text}");
    assert!(
        text.contains("still running"),
        "the developer needs to know nothing was lost: {text}"
    );
    assert!(out.is_empty());
}

#[test]
fn a_clean_baseline_is_one_progress_line_and_a_broken_one_keeps_watching() {
    // A project that does not currently compile is an ordinary state to start
    // in — refusing to start would be worse than useless.
    let mut out = Vec::new();
    let mut err = Vec::new();
    report_compile(
        &duet_dev::CompileOutcome {
            ok: true,
            diagnostics: Vec::new(),
            errors: None,
            dill: None,
            elapsed: Duration::from_millis(3800),
        },
        &mut out,
        &mut err,
    );
    assert!(
        String::from_utf8_lossy(&out).contains("baseline compile in 3800 ms"),
        "got {}",
        String::from_utf8_lossy(&out)
    );
    assert!(err.is_empty());

    let mut out = Vec::new();
    let mut err = Vec::new();
    report_compile(
        &duet_dev::CompileOutcome {
            ok: false,
            diagnostics: vec!["lib/main.dart:1:1: Error: nope".to_string()],
            errors: Some(1),
            dill: None,
            elapsed: Duration::from_millis(20),
        },
        &mut out,
        &mut err,
    );
    let text = String::from_utf8_lossy(&err).into_owned();
    assert!(text.contains("does not currently compile"), "got {text}");
    assert!(text.contains("Error: nope"), "got {text}");
    assert!(
        text.contains("Watching anyway"),
        "it must say the session is still usable: {text}"
    );
}

#[cfg(unix)]
#[test]
fn spawning_a_host_pipes_its_stdout_and_kills_it_on_drop() {
    // The whole fd-1 story in one test: the child's stdout is an ordinary
    // pipe, this process's own is untouched, and the announcement is read off
    // it exactly as an engine's would be.
    let (session, lines) = spawn_host(&[
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo starting; \
         echo 'flutter: The Dart VM service is listening on http://127.0.0.1:45671/'; \
         sleep 30"
            .to_string(),
    ])
    .expect("the host should start");

    let mut out = Vec::new();
    let url = await_announcement(&lines, &mut out, Duration::from_secs(10))
        .expect("the announcement should be read off the pipe");
    assert_eq!(url.websocket(), "ws://127.0.0.1:45671/ws");
    assert!(
        String::from_utf8_lossy(&out).contains("starting"),
        "earlier lines are passed through"
    );

    // Dropping the session must reap the child; a `duet dev` that left a
    // Flutter engine running would leak a window and a few hundred megabytes
    // every time the developer's project failed to build.
    drop(session);
}

#[test]
fn an_empty_host_command_is_refused_rather_than_spawned() {
    // The parser already rejects this, but `spawn_host` is reachable on its
    // own and `Command::new("")` is a confusing failure.
    let Err(e) = spawn_host(&[]) else {
        panic!("an empty command should not spawn");
    };
    assert_eq!(e.stage(), Stage::LocateVmService);
}

#[cfg(unix)]
#[test]
fn a_host_that_announces_and_then_exits_is_blamed_rather_than_the_network() {
    // The failure this rewrite exists for, and it is not exotic: a host that
    // announces its VM service and then finishes before the driver's baseline
    // compile does produces `Connection refused`, which points at the network
    // when the cause is a host that was never going to stay up. This crate's
    // own `flutter_state` example finishes in about a second and a half.
    let (mut session, _lines) = spawn_host(&[
        "/bin/sh".to_string(),
        "-c".to_string(),
        "exit 7".to_string(),
    ])
    .expect("the host should start");

    // Wait for it to be reapable, so the rewrite has something to observe.
    for _ in 0..200 {
        if matches!(session.child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let original = DevError::Io {
        stage: Stage::Connect,
        doing: "connecting to the Dart VM service",
        source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused"),
    };
    let rewritten = blame_the_host_if_it_died(&mut session, original);
    assert_eq!(
        rewritten.stage(),
        Stage::Connect,
        "the stage is preserved — it is still where the failure surfaced"
    );
    let text = rewritten.to_string();
    assert!(text.contains("exited"), "got {text}");
    assert!(
        text.contains("stays up"),
        "the advice belongs in it: {text}"
    );
}

#[cfg(unix)]
#[test]
fn a_host_that_is_still_alive_keeps_its_original_error() {
    // A live host refusing connections is a different problem, and rewriting
    // it would replace a true message with a false one.
    let (mut session, _lines) = spawn_host(&[
        "/bin/sh".to_string(),
        "-c".to_string(),
        "sleep 30".to_string(),
    ])
    .expect("the host should start");

    let rewritten = blame_the_host_if_it_died(
        &mut session,
        DevError::Timeout {
            stage: Stage::FindIsolate,
            after: Duration::from_secs(1),
        },
    );
    assert!(
        matches!(rewritten, DevError::Timeout { .. }),
        "a live host must not be blamed: {rewritten:?}"
    );
}
