//! Request shape, and every message a caller waiting on an id can receive.

use super::*;

#[test]
fn a_request_carries_the_four_fields_the_vm_service_requires() {
    let text = request(7, "getVM", json!({}));
    let parsed: Value = serde_json::from_str(&text).expect("a request must be valid JSON");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 7);
    assert_eq!(parsed["method"], "getVM");
    assert!(parsed.get("params").is_some(), "params must be present");
}

#[test]
fn our_reply_is_recognised_and_its_result_returned() {
    let reply = r#"{"jsonrpc":"2.0","id":2,"result":{"success":true}}"#;
    match correlate(reply, 2) {
        Correlation::Reply(result) => assert_eq!(result["success"], true),
        other => panic!("expected a reply, got {other:?}"),
    }
}

#[test]
fn an_id_sent_back_as_a_string_still_correlates() {
    // JSON-RPC permits string ids and implementations differ. Failing to match
    // here would hang the caller until its deadline rather than returning the
    // reply it was handed.
    match correlate(r#"{"id":"3","result":42}"#, 3) {
        Correlation::Reply(result) => assert_eq!(*result, json!(42)),
        other => panic!("a string id should correlate, got {other:?}"),
    }
}

#[test]
fn somebody_elses_traffic_is_ignored_rather_than_mistaken_for_a_reply() {
    // This is the entire reason correlation exists: the VM service pushes
    // events down the same socket, constantly. A driver that took the next
    // frame as its reply would break on the first GC.
    let others = [
        // A stream event: no `id` at all.
        r#"{"jsonrpc":"2.0","method":"streamNotify","params":{"streamId":"GC"}}"#,
        // A reply to a different request.
        r#"{"jsonrpc":"2.0","id":99,"result":{}}"#,
        // An id of the wrong JSON type.
        r#"{"jsonrpc":"2.0","id":null,"result":{}}"#,
        r#"{"jsonrpc":"2.0","id":[1],"result":{}}"#,
        // A string id that is not our number.
        r#"{"id":"nope","result":{}}"#,
        // Not an object at all.
        r#"[1,2,3]"#,
        r#""just a string""#,
    ];
    for message in others {
        assert_eq!(
            correlate(message, 1),
            Correlation::Other,
            "{message} is not a reply to id 1"
        );
    }
}

#[test]
fn an_error_reply_is_reported_with_its_code_and_details() {
    // The VM service puts the useful part in `data.details` — for a reload
    // that is the reason it was refused, which is the only thing the developer
    // wants to read.
    let reply = r#"{"jsonrpc":"2.0","id":4,"error":{
        "code":-32000,"message":"Service has disappeared",
        "data":{"details":"the isolate was collected"}}}"#;
    match correlate(reply, 4) {
        Correlation::Failed(text) => {
            assert!(
                text.contains("-32000"),
                "the code belongs in the message: {text}"
            );
            assert!(text.contains("Service has disappeared"), "got {text}");
            assert!(
                text.contains("the isolate was collected"),
                "the details are the useful part: {text}"
            );
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn an_error_reply_without_optional_fields_still_renders() {
    // Every combination of present/absent `code` and `data.details`, because
    // a formatter that indexed a missing field would panic while building an
    // error — the worst place for this crate to panic.
    let cases = [
        r#"{"id":1,"error":{"message":"plain"}}"#,
        r#"{"id":1,"error":{"code":-1,"message":"coded"}}"#,
        r#"{"id":1,"error":{"message":"detailed","data":{"details":"why"}}}"#,
        r#"{"id":1,"error":{}}"#,
    ];
    for message in cases {
        match correlate(message, 1) {
            Correlation::Failed(text) => {
                assert!(!text.is_empty(), "{message} should render something");
            }
            other => panic!("{message} should be a failure, got {other:?}"),
        }
    }
}

#[test]
fn a_reply_with_neither_result_nor_error_fails_immediately() {
    // It is definitely ours — the id matched — so treating it as somebody
    // else's would hang the caller for its full timeout instead of reporting
    // a malformed reply straight away.
    match correlate(r#"{"jsonrpc":"2.0","id":5}"#, 5) {
        Correlation::Failed(text) => assert!(
            text.contains("neither"),
            "the message should say what was missing: {text}"
        ),
        other => panic!("expected an immediate failure, got {other:?}"),
    }
}

#[test]
fn a_result_of_null_is_a_reply_not_a_missing_result() {
    // `ext.flutter.reassemble` and several other extension RPCs legitimately
    // answer with a null-ish result. Treating that as "no result" would turn
    // every successful reassemble into an error.
    match correlate(r#"{"id":6,"result":null}"#, 6) {
        Correlation::Reply(result) => assert!(result.is_null()),
        other => panic!("a null result is still a result, got {other:?}"),
    }
}

#[test]
fn a_frame_that_is_not_json_is_reported_as_such() {
    match correlate("not json at all", 1) {
        Correlation::Undecodable(text) => assert!(text.contains("not json")),
        other => panic!("expected undecodable, got {other:?}"),
    }
}

#[test]
fn an_undecodable_frame_does_not_carry_an_unbounded_amount_of_it_into_the_error() {
    // A peer that started streaming something enormous must not put all of it
    // in an error message.
    let huge = "x".repeat(100_000);
    match correlate(&huge, 1) {
        Correlation::Undecodable(text) => assert!(
            text.chars().count() <= 200,
            "the echo should be bounded, got {} chars",
            text.chars().count()
        ),
        other => panic!("expected undecodable, got {other:?}"),
    }
}
