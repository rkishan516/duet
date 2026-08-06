//! The whole cycle, end to end, with a fake compiler and a fake VM service.
//!
//! Both fakes are real processes and real sockets (see
//! [`crate::test_compiler`] and [`crate::test_server`]), so this drives every
//! branch of the orchestration — including the two easy-to-get-wrong ones:
//! `reject` after a failed compile, and *no* `reassemble` after a declined
//! reload.

use super::*;

#[test]
fn the_default_timeouts_are_generous_next_to_what_spike_c_measured() {
    // A deadline's job is to turn a hang into a report, not to police
    // performance. Spike C measured 9–22 ms recompiles and a 3.8 s first
    // compile, so every one of these has orders of magnitude of headroom.
    let timeouts = Timeouts::default();
    assert!(timeouts.baseline >= Duration::from_secs(30));
    assert!(timeouts.recompile >= Duration::from_secs(10));
    assert!(timeouts.rpc >= Duration::from_secs(10));
    assert!(timeouts.connect >= Duration::from_secs(5));
    assert!(
        timeouts.baseline > timeouts.recompile,
        "a first compile is much slower than an incremental one"
    );
}

#[test]
fn timings_default_to_zero() {
    let timings = Timings::default();
    assert_eq!(timings.total, Duration::ZERO);
    assert_eq!(timings.recompile, Duration::ZERO);
}

#[test]
fn applied_is_the_only_outcome_that_reports_new_code_is_running() {
    // `applied()` is what a caller branches on; a `Declined` reading as
    // applied would report success for a reload the VM refused.
    let report = ReloadReport {
        success: true,
        received_libraries: Some(2),
        saved_libraries: Some(752),
        notices: Vec::new(),
    };
    assert!(
        Reload::Applied {
            report: report.clone(),
            timings: Timings::default()
        }
        .applied()
    );
    assert!(
        !Reload::Declined {
            report,
            timings: Timings::default()
        }
        .applied()
    );
    assert!(
        !Reload::CompileFailed {
            diagnostics: vec!["Error: nope".to_string()],
            elapsed: Duration::ZERO
        }
        .applied()
    );
}

#[cfg(unix)]
mod cycle {
    use super::*;
    use crate::test_compiler::{Scratch, clean_script, config_for, failing_script};
    use crate::test_server::{Handshake, TestServer, read_text, write_text};
    use crate::{Stage, VmServiceUrl};
    use serde_json::Value;

    const QUICK: Duration = Duration::from_secs(10);

    fn timeouts() -> Timeouts {
        Timeouts {
            baseline: QUICK,
            connect: QUICK,
            rpc: QUICK,
            recompile: QUICK,
        }
    }

    /// A VM service that records every method it is asked for and answers with
    /// `reload_result` for `reloadSources`.
    fn vm_service(reload_result: &'static str) -> (TestServer, std::sync::mpsc::Receiver<String>) {
        let (sender, methods) = std::sync::mpsc::channel();
        let server = TestServer::start(Handshake::Correct, move |stream| {
            while let Some(request) = read_text(stream) {
                let parsed: Value = match serde_json::from_str(&request) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let id = parsed.get("id").cloned().unwrap_or(Value::Null);
                let method = parsed
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let _ = sender.send(method.clone());
                let result = match method.as_str() {
                    "getVM" => r#"{"isolates":[{"id":"isolates/42"}]}"#.to_string(),
                    "reloadSources" => reload_result.to_string(),
                    _ => r#"{"type":"_extensionType"}"#.to_string(),
                };
                write_text(stream, &format!(r#"{{"id":{id},"result":{result}}}"#));
            }
        });
        (server, methods)
    }

    fn drain(methods: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(method) = methods.recv_timeout(Duration::from_millis(200)) {
            seen.push(method);
        }
        seen
    }

    const SUCCESS: &str = r#"{"type":"ReloadReport","success":true,
        "details":{"receivedLibraryCount":2,"savedLibraryCount":752}}"#;
    const DECLINED: &str = r#"{"type":"ReloadReport","success":false,
        "details":{"notices":[{"message":"Const class cannot remove fields"}]}}"#;

    #[test]
    fn a_successful_cycle_runs_the_whole_sequence_in_order() {
        let scratch = Scratch::new("applied");
        let (server, methods) = vm_service(SUCCESS);
        let started = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &clean_script()),
            entrypoint: "package:duet_guest/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        })
        .expect("the session should start");
        assert!(started.baseline.ok, "the baseline compiled");
        let mut driver = started.driver;
        assert_eq!(driver.isolate(), &IsolateId("isolates/42".to_string()));
        assert_eq!(drain(&methods), vec!["getVM"], "start only calls getVM");

        let outcome = driver
            .reload(&["package:duet_guest/main.dart".to_string()])
            .expect("the reload should complete");
        let Reload::Applied { report, timings } = outcome else {
            panic!("expected Applied, got {outcome:?}");
        };
        assert!(report.success);
        assert_eq!(report.saved_libraries, Some(752));
        assert!(timings.total >= timings.recompile, "total covers the parts");

        assert_eq!(
            drain(&methods),
            vec!["reloadSources", "ext.flutter.reassemble"],
            "reassemble must follow a successful reloadSources, in that order"
        );
        driver.shutdown();
    }

    #[test]
    fn a_declined_reload_does_not_reassemble() {
        // Rebuilding the widget tree when no new code was loaded is at best
        // wasted work on the UI thread, and at worst hides the decline behind
        // a UI that visibly did something.
        let scratch = Scratch::new("declined");
        let (server, methods) = vm_service(DECLINED);
        let started = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &clean_script()),
            entrypoint: "package:x/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        })
        .expect("start");
        let mut driver = started.driver;
        let _ = drain(&methods);

        let outcome = driver.reload(&[]).expect("a decline is not a driver error");
        let Reload::Declined { report, timings } = outcome else {
            panic!("expected Declined, got {outcome:?}");
        };
        assert!(!report.success);
        assert_eq!(report.notices, vec!["Const class cannot remove fields"]);
        assert_eq!(
            timings.reassemble,
            Duration::ZERO,
            "no reassemble means no time spent in one"
        );
        assert_eq!(
            drain(&methods),
            vec!["reloadSources"],
            "reassemble must NOT be sent after a decline"
        );
        driver.shutdown();
    }

    #[test]
    fn a_compile_error_never_reaches_the_vm_service() {
        // The developer's typo is not the VM's problem, and asking it to
        // reload a kernel that was not produced would be a wrong answer at
        // best.
        let scratch = Scratch::new("compilefail");
        let dill = scratch.path.join("out/app.dill.incremental.dill");
        let (server, methods) = vm_service(SUCCESS);

        // A compiler that always fails — a project that does not currently
        // compile is a perfectly ordinary state to start `duet dev` in.
        let started = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &failing_script(&dill)),
            entrypoint: "package:x/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        })
        .expect("a project that does not compile is still a startable session");
        assert!(
            !started.baseline.ok,
            "the baseline is handed back so the caller can print it"
        );
        assert_eq!(started.baseline.diagnostics.len(), 2);

        let mut driver = started.driver;
        let _ = drain(&methods);
        let outcome = driver
            .reload(&[])
            .expect("a compile error is not a driver error");
        let Reload::CompileFailed { diagnostics, .. } = outcome else {
            panic!("expected CompileFailed, got {outcome:?}");
        };
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].contains("Expected ';'"));
        assert_eq!(
            drain(&methods),
            Vec::<String>::new(),
            "a failed compile must not touch the VM service"
        );
        driver.shutdown();
    }

    #[test]
    fn several_reloads_reuse_one_compiler_and_one_connection() {
        // The steady state of a dev session. A driver that only worked once
        // would fail on the second edit.
        let scratch = Scratch::new("repeat");
        let (server, methods) = vm_service(SUCCESS);
        let started = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &clean_script()),
            entrypoint: "package:x/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        })
        .expect("start");
        let mut driver = started.driver;
        let _ = drain(&methods);

        for round in 0..4 {
            let outcome = driver
                .reload(&[format!("package:x/edit{round}.dart")])
                .unwrap_or_else(|e| panic!("reload {round}: {e}"));
            assert!(outcome.applied(), "reload {round} should apply");
        }
        assert_eq!(
            drain(&methods).len(),
            8,
            "four reloads means four reloadSources and four reassembles"
        );
        driver.shutdown();
    }

    #[test]
    fn an_empty_invalidated_list_falls_back_to_the_entrypoint() {
        // A caller that knows something changed but not what should still get
        // a correct recompile rather than a malformed request.
        let scratch = Scratch::new("fallback");
        let (server, methods) = vm_service(SUCCESS);
        let started = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &clean_script()),
            entrypoint: "package:duet_guest/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        })
        .expect("start");
        let mut driver = started.driver;
        let _ = drain(&methods);
        assert!(
            driver.reload(&[]).expect("reload").applied(),
            "an empty list should still produce a reload"
        );
        driver.shutdown();
    }

    #[test]
    fn the_dill_uri_the_vm_is_given_is_a_file_uri_for_the_incremental_kernel() {
        // Pointing the VM at the wrong file is the failure that looks like
        // success: it reports `success: true` against a stale kernel and the
        // developer's edit silently does not appear.
        let scratch = Scratch::new("dilluri");
        let (sender, requests) = std::sync::mpsc::channel();
        let server = TestServer::start(Handshake::Correct, move |stream| {
            while let Some(request) = read_text(stream) {
                let parsed: Value = match serde_json::from_str(&request) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let id = parsed.get("id").cloned().unwrap_or(Value::Null);
                let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "reloadSources" {
                    let _ = sender.send(
                        parsed["params"]["rootLibUri"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
                let result = if method == "getVM" {
                    r#"{"isolates":[{"id":"isolates/1"}]}"#
                } else {
                    SUCCESS
                };
                write_text(stream, &format!(r#"{{"id":{id},"result":{result}}}"#));
            }
        });

        let config = config_for(&scratch, &clean_script());
        let expected = format!(
            "file://{}",
            scratch
                .path
                .join("out")
                .join("app.dill.incremental.dill")
                .display()
        );
        let started = ReloadDriver::start(DriverConfig {
            compiler: config,
            entrypoint: "package:x/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        })
        .expect("start");
        let mut driver = started.driver;
        driver.reload(&[]).expect("reload");

        let uri = requests.recv_timeout(QUICK).expect("a rootLibUri was sent");
        assert_eq!(
            uri, expected,
            "the VM must be pointed at the incremental dill"
        );
        driver.shutdown();
    }

    #[test]
    fn a_vm_service_that_is_not_there_fails_at_the_connect_stage() {
        // With the compiler already running — so this also proves the failed
        // start does not leak the child, which `Drop` covers.
        let scratch = Scratch::new("noservice");
        let port = crate::locate::free_port().expect("a free port");
        let Err(e) = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &clean_script()),
            entrypoint: "package:x/main.dart".to_string(),
            vm_service: VmServiceUrl::loopback(port),
            timeouts: Timeouts {
                connect: Duration::from_millis(500),
                ..timeouts()
            },
        }) else {
            panic!("nothing is listening on that port");
        };
        assert_eq!(e.stage(), Stage::Connect);
    }

    #[test]
    fn a_vm_with_no_isolate_fails_at_the_find_isolate_stage() {
        let scratch = Scratch::new("noisolate");
        let server = TestServer::start(Handshake::Correct, |stream| {
            while let Some(request) = read_text(stream) {
                let parsed: Value = match serde_json::from_str(&request) {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let id = parsed.get("id").cloned().unwrap_or(Value::Null);
                write_text(
                    stream,
                    &format!(r#"{{"id":{id},"result":{{"isolates":[]}}}}"#),
                );
            }
        });
        let Err(e) = ReloadDriver::start(DriverConfig {
            compiler: config_for(&scratch, &clean_script()),
            entrypoint: "package:x/main.dart".to_string(),
            vm_service: server.url(),
            timeouts: timeouts(),
        }) else {
            panic!("a VM with no isolates cannot be reloaded");
        };
        assert_eq!(e.stage(), Stage::FindIsolate);
    }
}
