//! The compiler client, against a real child process that speaks the protocol.
//!
//! See [`crate::test_compiler`] for why the child is a shell script rather
//! than a real `frontend_server`, and why these are `#[cfg(unix)]`.

use super::*;

#[cfg(unix)]
mod driving {
    use super::*;
    use crate::test_compiler::{
        Scratch, clean_script, clean_with_dill_script, config_for, dies_after_one_script,
        dies_immediately_script, failing_script, garbage_script, noisy_accept_script,
        silent_script,
    };

    const QUICK: Duration = Duration::from_secs(10);

    #[test]
    fn a_clean_compile_reports_success_with_no_diagnostics() {
        let scratch = Scratch::new("clean");
        let config = config_for(&scratch, &clean_script());
        let mut compiler = FrontendServer::spawn(&config).expect("the fake should start");

        let outcome = compiler
            .compile("package:duet_guest/main.dart", QUICK)
            .expect("the compile should complete");
        assert!(outcome.ok, "a clean compile succeeded: {outcome:?}");
        assert!(
            outcome.diagnostics.is_empty(),
            "got {:?}",
            outcome.diagnostics
        );
        assert_eq!(outcome.errors, None, "this SDK shape reports no count");
        compiler.shutdown();
    }

    #[test]
    fn a_failing_compile_is_an_ok_outcome_carrying_its_diagnostics() {
        // The single most important behaviour of this type. A syntax error is
        // the most common thing that happens in a dev loop and it is not a
        // failure of the driver — a driver that returned `Err` here would end
        // the session on every typo.
        let scratch = Scratch::new("failing");
        let dill = scratch.path.join("out/app.dill.incremental.dill");
        let config = config_for(&scratch, &failing_script(&dill));
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");

        let outcome = compiler
            .compile("package:x/main.dart", QUICK)
            .expect("a compile error is not a driver error");
        assert!(!outcome.ok);
        assert_eq!(outcome.errors, Some(2), "the count comes from the compiler");
        assert_eq!(outcome.diagnostics.len(), 2);
        assert!(
            outcome.diagnostics[0].contains("Expected ';'"),
            "the developer needs the real text: {:?}",
            outcome.diagnostics
        );
        assert_eq!(
            outcome.dill.as_deref(),
            Some(dill.display().to_string().as_str())
        );
        compiler.shutdown();
    }

    #[test]
    fn the_compilers_own_error_count_wins_over_the_diagnostic_heuristic() {
        // A zero count with diagnostics present must read as success: warnings
        // and `Context:` lines are diagnostics too, and refusing to reload
        // because of a warning would be worse than useless.
        let scratch = Scratch::new("countwins");
        let dill = scratch.path.join("out/app.dill.incremental.dill");
        let config = config_for(&scratch, &clean_with_dill_script(&dill));
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        let outcome = compiler
            .compile("package:x/main.dart", QUICK)
            .expect("compile");
        assert!(outcome.ok);
        assert_eq!(outcome.errors, Some(0));
        compiler.shutdown();
    }

    #[test]
    fn a_recompile_exchange_completes_and_can_repeat() {
        // Every reload after the first reuses the same process, so a client
        // that only worked once would fail on the second edit — the worst kind
        // of bug to find.
        let scratch = Scratch::new("repeat");
        let config = config_for(&scratch, &clean_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        compiler
            .compile("package:x/main.dart", QUICK)
            .expect("baseline");
        compiler.accept().expect("accept");

        for round in 0..5 {
            let outcome = compiler
                .recompile(&["package:x/main.dart".to_string()], QUICK)
                .unwrap_or_else(|e| panic!("recompile {round} should complete: {e}"));
            assert!(outcome.ok, "recompile {round} succeeded");
            compiler.accept().expect("accept");
        }
        compiler.shutdown();
    }

    #[test]
    fn a_delayed_accept_echo_does_not_corrupt_the_next_exchange() {
        // The real quirk Spike C recorded: `accept` sometimes echoes a
        // confirmation that arrives interleaved with the *next* command's
        // output. Counting it as a diagnostic would make every clean compile
        // after an accept look like it said something.
        let scratch = Scratch::new("noisyaccept");
        let config = config_for(&scratch, &noisy_accept_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        compiler
            .compile("package:x/main.dart", QUICK)
            .expect("baseline");
        compiler.accept().expect("accept");

        let outcome = compiler
            .recompile(&["package:x/main.dart".to_string()], QUICK)
            .expect("the next recompile should still parse");
        assert!(outcome.ok, "the stray echo must not be read as an error");
        assert!(
            outcome.diagnostics.is_empty(),
            "the stray echo must not be read as a diagnostic: {:?}",
            outcome.diagnostics
        );
        compiler.shutdown();
    }

    #[test]
    fn a_compiler_that_never_starts_reports_its_stderr() {
        // A bad `--sdk-root` or a snapshot from another SDK. The reason is in
        // stderr, which Spike C let go to the terminal and therefore could not
        // put in an error.
        let scratch = Scratch::new("dies");
        let config = config_for(&scratch, &dies_immediately_script());
        let mut compiler = FrontendServer::spawn(&config).expect("the process starts, then exits");

        let Err(e) = compiler.compile("package:x/main.dart", QUICK) else {
            panic!("a dead compiler cannot compile");
        };
        let DevError::CompilerExited { status, stderr, .. } = &e else {
            panic!("expected CompilerExited, got {e:?}");
        };
        assert_eq!(*status, Some(253), "the exit status should be collected");
        assert!(
            stderr.contains("flutter_patched_sdk"),
            "the stderr explains it: {stderr}"
        );
        assert!(
            stderr.contains("--sdk-root"),
            "and a compiler that never answered gets the extra hint: {stderr}"
        );
        assert_eq!(e.stage(), Stage::BaselineCompile);
    }

    #[test]
    fn a_compiler_that_dies_mid_session_is_reported_rather_than_hanging() {
        // Harder than dying at startup, because the driver has already seen it
        // work. Its stdout closing is an ordinary EOF; only checking for it
        // turns that into a usable report.
        let scratch = Scratch::new("diesafter");
        let config = config_for(&scratch, &dies_after_one_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        compiler
            .compile("package:x/main.dart", QUICK)
            .expect("the first exchange works");

        let Err(e) = compiler.recompile(&["package:x/main.dart".to_string()], QUICK) else {
            panic!("a dead compiler cannot recompile");
        };
        assert!(
            matches!(
                e,
                DevError::CompilerExited {
                    stage: Stage::Recompile,
                    ..
                }
            ),
            "got {e:?}"
        );
        assert!(
            e.to_string().contains("crashed while recompiling"),
            "its stderr should be carried: {e}"
        );
    }

    #[test]
    fn writing_to_a_dead_compiler_reports_the_death_not_a_broken_pipe() {
        // `accept` has no reply to wait on, so its only signal is the write
        // failing. `BrokenPipe` would send a developer looking at their
        // filesystem.
        let scratch = Scratch::new("writedead");
        let config = config_for(&scratch, &dies_immediately_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        // Give the child a moment to exit, so the pipe is genuinely broken.
        std::thread::sleep(Duration::from_millis(200));
        // The first write may be buffered by the OS; the loop makes the test
        // about the eventual report rather than about pipe buffering.
        let mut last = Ok(());
        for _ in 0..200 {
            last = compiler.accept();
            if last.is_err() {
                break;
            }
        }
        let Err(e) = last else {
            panic!("writing to a dead compiler should eventually fail");
        };
        assert!(matches!(e, DevError::CompilerExited { .. }), "got {e:?}");
    }

    #[test]
    fn a_compiler_that_never_answers_times_out_at_the_right_stage() {
        // Not hanging is the point. `read_line` on a pipe would wait forever.
        let scratch = Scratch::new("silent");
        let config = config_for(&scratch, &silent_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        let started = Instant::now();
        let Err(e) = compiler.compile("package:x/main.dart", Duration::from_millis(300)) else {
            panic!("a silent compiler should not complete");
        };
        assert!(
            matches!(
                e,
                DevError::Timeout {
                    stage: Stage::BaselineCompile,
                    ..
                }
            ),
            "got {e:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it should give up promptly, took {:?}",
            started.elapsed()
        );
        compiler.shutdown();
    }

    #[test]
    fn a_compiler_that_talks_but_never_says_result_reports_a_protocol_change() {
        // A future SDK changing the line format must not produce a wrong
        // answer — and must not be reported as a hang either, because the
        // compiler is demonstrably alive and talking. "Never a `result` line"
        // points straight at the protocol; "timed out" would send the
        // developer hunting for a wedged process.
        let scratch = Scratch::new("garbage");
        let config = config_for(&scratch, &garbage_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        let Err(e) = compiler.compile("package:x/main.dart", Duration::from_millis(400)) else {
            panic!("output with no result line is not a completed block");
        };
        assert!(
            matches!(
                e,
                DevError::CompilerProtocol {
                    stage: Stage::BaselineCompile,
                    ..
                }
            ),
            "got {e:?}"
        );
        assert!(
            e.to_string().contains("result <boundary>"),
            "the message should name what was missing: {e}"
        );
        compiler.shutdown();
    }

    #[test]
    fn a_compiler_that_says_nothing_at_all_is_reported_as_a_hang() {
        // The other half of the distinction above: silence really is a hang,
        // and must keep reporting as one.
        let scratch = Scratch::new("silent-vs-garbage");
        let config = config_for(&scratch, &silent_script());
        let mut compiler = FrontendServer::spawn(&config).expect("spawn");
        let Err(e) = compiler.compile("package:x/main.dart", Duration::from_millis(300)) else {
            panic!("a silent compiler should not complete");
        };
        assert!(matches!(e, DevError::Timeout { .. }), "got {e:?}");
        compiler.shutdown();
    }

    #[test]
    fn the_incremental_dill_sits_next_to_the_output_dill() {
        // Documented nowhere; found by Spike C by looking in the working
        // directory after a successful recompile. It is what `reloadSources`
        // is pointed at, so a wrong name is a reload against a stale kernel.
        let scratch = Scratch::new("dillpath");
        let config = config_for(&scratch, &clean_script());
        let compiler = FrontendServer::spawn(&config).expect("spawn");
        assert_eq!(
            compiler.incremental_dill(),
            scratch.path.join("out").join("app.dill.incremental.dill")
        );
        compiler.shutdown();
    }

    #[test]
    fn spawning_creates_the_output_directory() {
        // `frontend_server` does not create it and fails obscurely without it.
        let scratch = Scratch::new("mkdir");
        let config = config_for(&scratch, &clean_script());
        assert!(!scratch.path.join("out").exists(), "not there beforehand");
        let compiler = FrontendServer::spawn(&config).expect("spawn");
        assert!(
            scratch.path.join("out").is_dir(),
            "the output directory should have been created"
        );
        compiler.shutdown();
    }

    #[test]
    fn a_compiler_that_cannot_be_started_at_all_is_an_io_error() {
        let scratch = Scratch::new("nobinary");
        let mut config = config_for(&scratch, &clean_script());
        config.sdk.dartaotruntime = std::path::PathBuf::from("/definitely/not/a/binary");
        let Err(e) = FrontendServer::spawn(&config) else {
            panic!("a missing runtime should not spawn");
        };
        assert_eq!(e.stage(), Stage::SpawnCompiler);
        assert!(e.to_string().contains("frontend_server"), "got {e}");
    }

    #[test]
    fn dropping_the_client_reaps_the_child() {
        // An orphaned compiler holds hundreds of megabytes. `Drop` is what
        // covers the `?` paths that never reach `shutdown`.
        let scratch = Scratch::new("reap");
        let config = config_for(&scratch, &silent_script());
        let compiler = FrontendServer::spawn(&config).expect("spawn");
        drop(compiler);
        // Reaching here without hanging is the assertion: `Drop` kills and
        // waits, so a child that ignored it would block this test forever.
    }
}

#[test]
fn a_file_uri_is_what_reload_sources_accepts() {
    // `rootLibUri` must be a `file://` URI; a bare path is silently rejected
    // by the VM in a way that looks like a successful reload of nothing.
    assert_eq!(
        file_uri(Path::new("/tmp/duet-dev/out.dill.incremental.dill")),
        "file:///tmp/duet-dev/out.dill.incremental.dill"
    );
}

#[test]
fn a_windows_drive_path_becomes_a_well_formed_file_uri() {
    // A drive path has no leading slash and carries backslashes, so the naive
    // glue produced `file://C:\...` — the drive letter parsed as a URI host —
    // and every `reloadSources` on Windows was declined with no stated
    // reason (found by the Windows hot_reload example). The transformation is
    // pure string logic, so this pins it on every platform, not just the one
    // where the input shape occurs naturally.
    assert_eq!(
        file_uri(Path::new(r"C:\Users\dev\out.dill.incremental.dill")),
        "file:///C:/Users/dev/out.dill.incremental.dill"
    );
}
