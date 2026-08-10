//! The binary itself: argv, exit codes, and the bytes on stdout.
//!
//! `src/serve.rs` tests the session against in-memory streams, which is where
//! the protocol behaviour belongs. This drives the **process**, because that is
//! what a guest actually spawns and because the parts only a process has —
//! argument parsing, exit codes, stderr, and a real pipe — have no other cover.
//!
//! `CARGO_BIN_EXE_duet-host-stdio` is set by Cargo for this crate's integration
//! tests, so the binary under test is always the one just built rather than
//! whatever happens to be on `PATH`.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs the host with `arguments`, feeding it `input`.
fn run(arguments: &[&str], input: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_duet-host-stdio"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the host binary should start");
    // A host that rejects its arguments exits without ever reading stdin, and
    // on a loaded machine it can be gone before this write lands — the pipe
    // then reports BrokenPipe (a CI runner lost exactly that race; a fast
    // machine's write always slips into the pipe buffer first, which is why
    // no local run ever saw it). That is not a fault in the thing under test:
    // every test that expects a *served* session asserts on stdout and the
    // exit code, which a wrongly-dead host fails loudly. Any other write
    // error is still a real fault worth stopping on.
    let written = child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(input.as_bytes());
    if let Err(e) = written {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe,
            "writing to the host should succeed or find it already exited: {e}"
        );
    }
    let done = child
        .wait_with_output()
        .expect("the host should exit on end of input");
    (
        done.status.code().unwrap_or(-1),
        String::from_utf8(done.stdout).expect("this host only ever writes UTF-8"),
        String::from_utf8_lossy(&done.stderr).into_owned(),
    )
}

#[test]
fn a_named_fixture_serves_the_protocol_and_exits_cleanly() {
    // The whole harness contract in one transcript: seeded store, one line in,
    // one line out, clean exit at end of input.
    let (code, out, err) = run(
        &["app"],
        "{\"kind\":\"get\",\"id\":\"1\",\"path\":\"editor.zoom\"}\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out,
        "{\"id\":\"1\",\"kind\":\"value\",\"value\":{\"t\":\"f\",\"v\":0.0}}\n"
    );
    assert_eq!(err, "", "stdout is the protocol; stderr must stay empty");
}

#[test]
fn a_subscription_pushes_before_the_reply_that_caused_it() {
    // The determinism claim, observed through a real pipe rather than through
    // an in-memory writer — which is where a buffering mistake would hide.
    let (code, out, err) = run(
        &["app"],
        "{\"kind\":\"subscribe\",\"id\":\"1\",\"path\":\"counter\"}\n\
         {\"kind\":\"set\",\"id\":\"2\",\"path\":\"counter\",\"value\":{\"t\":\"i\",\"v\":\"42\"}}\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "got {out:?}");
    assert!(lines[0].contains("\"subscribed\""), "{}", lines[0]);
    assert_eq!(
        lines[1],
        "{\"kind\":\"notification\",\"notification\":{\"patch\":{\"path\":\"counter\",\
         \"value\":{\"t\":\"i\",\"v\":\"42\"}},\"subscriber\":\"0\",\"subscription\":\"0\"}}"
    );
    assert_eq!(lines[2], "{\"id\":\"2\",\"kind\":\"done\"}");
}

#[test]
fn every_fixture_can_be_named() {
    for name in duet_host_stdio::names() {
        let (code, out, err) = run(&[name], "{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\"}\n");
        assert_eq!(code, 0, "{name}: stderr: {err}");
        assert!(out.contains("\"kind\":\"value\""), "{name}: {out}");
    }
}

#[test]
fn no_argument_is_a_usage_failure_that_lists_the_fixtures() {
    let (code, out, err) = run(&[], "");
    assert_eq!(code, 2, "usage is exit 2, distinct from a run that failed");
    assert_eq!(
        out, "",
        "a usage failure must not put anything on the protocol stream"
    );
    assert!(err.contains("usage: duet-host-stdio <schema>"), "{err}");
    for name in duet_host_stdio::names() {
        assert!(
            err.contains(name),
            "the usage message must list {name}: {err}"
        );
    }
}

#[test]
fn too_many_arguments_is_a_usage_failure() {
    let (code, _, err) = run(&["app", "wide"], "");
    assert_eq!(code, 2);
    assert!(err.contains("usage"), "{err}");
}

#[test]
fn a_flag_is_a_usage_failure_rather_than_a_fixture_name() {
    // `--help` is not implemented, and treating it as a fixture name would
    // produce "no schema fixture named --help" — technically true and useless.
    let (code, _, err) = run(&["--help"], "");
    assert_eq!(code, 2);
    assert!(err.contains("usage"), "{err}");
}

#[test]
fn an_unknown_fixture_fails_with_a_diagnostic_and_serves_nothing() {
    let (code, out, err) = run(&["nope"], "{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\"}\n");
    assert_eq!(code, 1, "a failed run is exit 1, distinct from bad usage");
    assert_eq!(out, "");
    assert!(err.contains("no schema fixture named \"nope\""), "{err}");
    assert!(
        err.contains("app"),
        "the refusal must list what would work: {err}"
    );
}

#[test]
fn a_malformed_line_is_answered_and_the_session_continues() {
    // Recovery across the process boundary: one bad message must not cost a
    // guest its connection, and a host that closed the stream instead would
    // look, from the guest's side, exactly like a hang.
    let (code, out, err) = run(
        &["app"],
        "not json\n{\"kind\":\"get\",\"id\":\"2\",\"path\":\"counter\"}\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "got {out:?}");
    assert!(lines[0].contains("\"failed\""), "{}", lines[0]);
    assert_eq!(
        lines[1],
        "{\"id\":\"2\",\"kind\":\"value\",\"value\":{\"t\":\"i\",\"v\":\"0\"}}"
    );
}

#[test]
fn an_unterminated_final_line_is_served_before_the_stream_ends() {
    let (code, out, err) = run(
        &["app"],
        "{\"kind\":\"get\",\"id\":\"1\",\"path\":\"counter\"}",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(
        out,
        "{\"id\":\"1\",\"kind\":\"value\",\"value\":{\"t\":\"i\",\"v\":\"0\"}}\n"
    );
}

#[test]
fn empty_input_exits_immediately_and_says_nothing() {
    let (code, out, err) = run(&["app"], "");
    assert_eq!(code, 0);
    assert_eq!(out, "");
    assert_eq!(err, "");
}

#[test]
fn a_line_past_the_limit_is_refused_without_being_echoed() {
    // The allocation bound, through the process: 8 MiB of one unterminated
    // line against a 1 MiB ceiling, then a good request to prove the session
    // survived it.
    let mut input = String::with_capacity(9 * 1024 * 1024);
    input.push_str(&"{".repeat(8 * 1024 * 1024));
    input.push('\n');
    input.push_str("{\"kind\":\"get\",\"id\":\"9\",\"path\":\"counter\"}\n");

    let (code, out, err) = run(&["app"], &input);
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "got {} lines", lines.len());
    assert!(lines[0].contains("\"failed\""), "{}", lines[0]);
    assert!(
        lines[0].len() < 512,
        "the refusal echoed {} bytes of an 8 MiB line",
        lines[0].len()
    );
    assert!(
        lines[0].contains(&duet_host_stdio::MAX_REQUEST_BYTES.to_string()),
        "the refusal must name the limit it enforced: {}",
        lines[0]
    );
    assert_eq!(
        lines[1],
        "{\"id\":\"9\",\"kind\":\"value\",\"value\":{\"t\":\"i\",\"v\":\"0\"}}"
    );
}

#[test]
fn a_closed_stdout_is_not_reported_as_a_crash() {
    // A guest that exits first closes the pipe. That is how a session normally
    // ends, and exiting non-zero for it would make every clean disconnect look
    // like a failure to whatever supervises this process.
    let mut child = Command::new(env!("CARGO_BIN_EXE_duet-host-stdio"))
        .arg("app")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the host binary should start");
    {
        let stdin = child.stdin.as_mut().expect("stdin was piped");
        for _ in 0..100 {
            let _ = stdin.write_all(b"{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\"}\n");
        }
    }
    let done = child.wait_with_output().expect("the host should exit");
    assert_eq!(
        done.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}
