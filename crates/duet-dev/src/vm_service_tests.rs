//! The three RPCs, against a scripted VM service on a real socket.

use super::*;
use crate::test_server::{Handshake, TestServer, read_text, write_text};

const QUICK: Duration = Duration::from_secs(5);

/// A server that answers each request by looking up its `method` in `replies`
/// and echoing the request's own id back.
///
/// Echoing the id rather than hard-coding one is what makes these tests
/// exercise real correlation instead of a fixed number that would pass even if
/// correlation were broken.
fn scripted(replies: Vec<(&'static str, String)>) -> TestServer {
    TestServer::start(Handshake::Correct, move |stream| {
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
            let Some((_, body)) = replies.iter().find(|(m, _)| *m == method) else {
                write_text(
                    stream,
                    &format!(
                        r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"Method not found: {method}"}}}}"#
                    ),
                );
                continue;
            };
            write_text(
                stream,
                &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{body}}}"#),
            );
        }
    })
}

fn get_vm_reply() -> String {
    r#"{"type":"VM","isolates":[{"type":"@Isolate","id":"isolates/7546815380299399","name":"main"}]}"#
        .to_string()
}

/// The `reloadSources` result Spike C captured verbatim from a real reload.
fn real_reload_reply() -> String {
    r#"{"type":"ReloadReport","success":true,"details":{
        "loadedLibraryCount":1,"finalLibraryCount":753,"receivedClassesCount":8,
        "receivedLibrariesBytes":22912,"receivedLibraryCount":2,
        "receivedProceduresCount":2,"savedLibraryCount":752,"shapeChangeMappings":[]}}"#
        .to_string()
}

#[test]
fn get_vm_finds_the_main_isolate() {
    let server = scripted(vec![("getVM", get_vm_reply())]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    assert_eq!(
        client
            .main_isolate(QUICK)
            .expect("an isolate should be found"),
        IsolateId("isolates/7546815380299399".to_string())
    );
}

#[test]
fn a_vm_with_no_isolates_is_reported_with_the_count() {
    // Happens if the driver connects before `runWithEntrypoint` has produced
    // an isolate. "0 isolates" tells the developer to wait; a generic parse
    // error would not.
    let server = scripted(vec![(
        "getVM",
        r#"{"type":"VM","isolates":[]}"#.to_string(),
    )]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let Err(e) = client.main_isolate(QUICK) else {
        panic!("no isolates should not produce an id");
    };
    assert_eq!(e.stage(), Stage::FindIsolate);
    assert!(e.to_string().contains("0 isolate"), "got {e}");
}

#[test]
fn a_get_vm_reply_of_the_wrong_shape_is_reported_not_guessed() {
    for reply in [
        r#"{"type":"VM"}"#,
        r#"{"isolates":"not an array"}"#,
        r#"{"isolates":[{"name":"no id here"}]}"#,
        r#"{"isolates":[{"id":42}]}"#,
    ] {
        let server = scripted(vec![("getVM", reply.to_string())]);
        let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
        assert!(
            client.main_isolate(QUICK).is_err(),
            "{reply} should not yield an isolate"
        );
    }
}

#[test]
fn reload_sources_reads_the_report_spike_c_captured() {
    // The counts are what distinguish an incremental reload from a full one:
    // 2 libraries received against 752 saved. A driver that only checked
    // `success` could not tell those apart, and a silent full reload would
    // lose exactly the heap state this whole feature exists to preserve.
    let server = scripted(vec![("reloadSources", real_reload_reply())]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let report = client
        .reload_sources(
            &IsolateId("isolates/1".to_string()),
            "file:///tmp/out.dill.incremental.dill",
            QUICK,
        )
        .expect("the reload should be reported");
    assert!(report.success);
    assert_eq!(report.received_libraries, Some(2));
    assert_eq!(report.saved_libraries, Some(752));
    assert!(report.notices.is_empty());
}

#[test]
fn reload_sources_never_sends_force() {
    // The most expensive lesson in Spike C: `"force": true` aborts the Dart VM
    // inside its own C++ runtime — the process dies, there is no error to
    // catch. `ReloadSources` has no such field, and this asserts the encoded
    // request agrees, so reintroducing the crash means editing a struct and
    // deleting this test rather than forgetting a default.
    let (sender, received) = std::sync::mpsc::channel();
    let server = TestServer::start(Handshake::Correct, move |stream| {
        while let Some(request) = read_text(stream) {
            let _ = sender.send(request.clone());
            let parsed: Value = serde_json::from_str(&request).unwrap_or(Value::Null);
            let id = parsed.get("id").cloned().unwrap_or(Value::Null);
            write_text(
                stream,
                &format!(r#"{{"id":{id},"result":{}}}"#, real_reload_reply()),
            );
        }
    });
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    client
        .reload_sources(
            &IsolateId("isolates/1".to_string()),
            "file:///tmp/x.dill",
            QUICK,
        )
        .expect("the reload should succeed");

    let request = received.recv_timeout(QUICK).expect("the request was sent");
    assert!(
        !request.contains("force"),
        "reloadSources must never carry `force`; sent: {request}"
    );
    let parsed: Value = serde_json::from_str(&request).expect("valid JSON");
    assert_eq!(parsed["method"], "reloadSources");
    assert_eq!(parsed["params"]["isolateId"], "isolates/1");
    assert_eq!(parsed["params"]["rootLibUri"], "file:///tmp/x.dill");
    assert_eq!(
        parsed["params"].as_object().map(|o| o.len()),
        Some(2),
        "exactly two params, so nothing can be smuggled in: {request}"
    );
}

#[test]
fn a_declined_reload_is_an_ok_report_rather_than_an_error() {
    // A change hot reload cannot express is an ordinary event in a dev loop —
    // the developer restarts. It is not a driver failure, so it must not be
    // an `Err` that stops the session.
    let server = scripted(vec![(
        "reloadSources",
        r#"{"type":"ReloadReport","success":false,"details":{
            "notices":[{"message":"Const class cannot remove fields"}]}}"#
            .to_string(),
    )]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let report = client
        .reload_sources(&IsolateId("isolates/1".to_string()), "file:///tmp/x", QUICK)
        .expect("a decline is still a successful RPC");
    assert!(!report.success);
    assert_eq!(report.notices, vec!["Const class cannot remove fields"]);
}

#[test]
fn a_report_missing_optional_details_still_reads() {
    // The VM's `details` object has gained fields across SDK versions.
    // Requiring all of them would break on an upgrade for no reason.
    let server = scripted(vec![("reloadSources", r#"{"success":true}"#.to_string())]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let report = client
        .reload_sources(&IsolateId("isolates/1".to_string()), "file:///tmp/x", QUICK)
        .expect("a minimal report should still read");
    assert!(report.success);
    assert_eq!(report.received_libraries, None);
    assert_eq!(report.saved_libraries, None);
}

#[test]
fn a_report_without_success_is_refused_rather_than_assumed() {
    // Defaulting a missing `success` either way would be a lie. The driver
    // would either skip a reassemble it needed, or claim a reload happened.
    let server = scripted(vec![(
        "reloadSources",
        r#"{"type":"ReloadReport","details":{}}"#.to_string(),
    )]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let Err(e) = client.reload_sources(&IsolateId("isolates/1".to_string()), "file:///x", QUICK)
    else {
        panic!("a report with no `success` must not be interpreted");
    };
    assert!(e.to_string().contains("unreadable report"), "got {e}");
}

#[test]
fn an_rpc_error_is_reported_with_the_method_that_failed() {
    // The server here knows no methods, so every call gets -32601.
    let server = scripted(vec![]);
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let Err(e) = client.main_isolate(QUICK) else {
        panic!("an unknown method should fail");
    };
    let text = e.to_string();
    assert!(text.contains("getVM"), "the method belongs in it: {text}");
    assert!(text.contains("-32601"), "and the code: {text}");
}

#[test]
fn reassemble_is_sent_with_the_isolate_and_accepts_an_extension_reply() {
    // Flutter answers extension RPCs with `{"type":"_extensionType"}`, which
    // is not a shape with a `success` field — treating it as a failure would
    // make every successful reload look broken.
    let (sender, received) = std::sync::mpsc::channel();
    let server = TestServer::start(Handshake::Correct, move |stream| {
        while let Some(request) = read_text(stream) {
            let _ = sender.send(request.clone());
            let parsed: Value = serde_json::from_str(&request).unwrap_or(Value::Null);
            let id = parsed.get("id").cloned().unwrap_or(Value::Null);
            write_text(
                stream,
                &format!(
                    r#"{{"id":{id},"result":{{"method":"ext.flutter.reassemble","type":"_extensionType"}}}}"#
                ),
            );
        }
    });
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    client
        .reassemble(&IsolateId("isolates/9".to_string()), QUICK)
        .expect("reassemble should succeed");

    let request = received.recv_timeout(QUICK).expect("the request was sent");
    let parsed: Value = serde_json::from_str(&request).expect("valid JSON");
    assert_eq!(parsed["method"], "ext.flutter.reassemble");
    assert_eq!(parsed["params"]["isolateId"], "isolates/9");
}

#[test]
fn events_interleaved_with_replies_are_skipped() {
    // The real reason correlation exists. A live VM service pushes GC,
    // logging and isolate events constantly, and a reload happens in the
    // middle of that traffic.
    let server = TestServer::start(Handshake::Correct, |stream| {
        let Some(request) = read_text(stream) else {
            return;
        };
        let parsed: Value = serde_json::from_str(&request).unwrap_or(Value::Null);
        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        for noise in [
            r#"{"jsonrpc":"2.0","method":"streamNotify","params":{"streamId":"GC"}}"#,
            r#"{"jsonrpc":"2.0","method":"streamNotify","params":{"streamId":"Stdout"}}"#,
            r#"{"jsonrpc":"2.0","id":99999,"result":{"stale":true}}"#,
        ] {
            write_text(stream, noise);
        }
        write_text(
            stream,
            &format!(r#"{{"id":{id},"result":{}}}"#, get_vm_reply()),
        );
    });
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    assert_eq!(
        client
            .main_isolate(QUICK)
            .expect("the reply should be found"),
        IsolateId("isolates/7546815380299399".to_string())
    );
}

#[test]
fn successive_calls_use_distinct_ids() {
    // If ids were reused, a late reply to an abandoned call would be mistaken
    // for the current one — which on a reload path means reporting the
    // previous reload's result for this one.
    let (sender, received) = std::sync::mpsc::channel();
    let server = TestServer::start(Handshake::Correct, move |stream| {
        while let Some(request) = read_text(stream) {
            let parsed: Value = serde_json::from_str(&request).unwrap_or(Value::Null);
            let id = parsed.get("id").cloned().unwrap_or(Value::Null);
            let _ = sender.send(id.clone());
            write_text(
                stream,
                &format!(r#"{{"id":{id},"result":{}}}"#, get_vm_reply()),
            );
        }
    });
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    for _ in 0..4 {
        client.main_isolate(QUICK).expect("call should succeed");
    }
    let mut ids: Vec<u64> = (0..4)
        .map(|_| {
            received
                .recv_timeout(QUICK)
                .expect("an id")
                .as_u64()
                .expect("ids are numbers")
        })
        .collect();
    let seen = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), seen, "every request must use a fresh id");
}

#[test]
fn a_vm_that_never_answers_times_out_at_the_right_stage() {
    // The whole point of the deadlines. Each RPC reports its own stage so
    // "the reload hung" is always "it hung at reload-sources".
    let server = TestServer::start(Handshake::Correct, |stream| {
        std::thread::sleep(Duration::from_secs(10));
        let _ = stream;
    });
    let mut client = VmServiceClient::connect(&server.url(), QUICK).expect("connect");
    let short = Duration::from_millis(200);

    let Err(e) = client.main_isolate(short) else {
        panic!("a silent VM should not answer");
    };
    assert!(
        matches!(
            e,
            DevError::Timeout {
                stage: Stage::FindIsolate,
                ..
            }
        ),
        "got {e:?}"
    );

    let Err(e) = client.reload_sources(&IsolateId("i".to_string()), "file:///x", short) else {
        panic!("a silent VM should not answer");
    };
    assert!(
        matches!(
            e,
            DevError::Timeout {
                stage: Stage::ReloadSources,
                ..
            }
        ),
        "got {e:?}"
    );

    let Err(e) = client.reassemble(&IsolateId("i".to_string()), short) else {
        panic!("a silent VM should not answer");
    };
    assert!(
        matches!(
            e,
            DevError::Timeout {
                stage: Stage::Reassemble,
                ..
            }
        ),
        "got {e:?}"
    );
}
