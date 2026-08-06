//! Tests for [`super::Session`] and [`super::serve`].

use super::*;
use std::io::Cursor;

/// Runs `requests` — one per line — against a fresh session and returns the
/// lines it wrote.
fn transcript(fixture: &str, requests: &[&str]) -> Vec<String> {
    let session = Session::open(fixture).expect("the fixture should open");
    let mut input = Cursor::new(requests.join("\n").into_bytes());
    let mut output: Vec<u8> = Vec::new();
    serve(&session, &mut input, &mut output).expect("a cursor cannot fail");
    session.shutdown().expect("the store should stop");
    String::from_utf8(output)
        .expect("this host only ever writes UTF-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// Parses one transcript line as JSON.
fn json(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{line} should be valid JSON: {e}"))
}

#[test]
fn the_store_starts_at_the_schemas_seed() {
    // The claim the whole harness stands on: a guest reading before writing
    // sees the shape its schema describes, not an empty store. An `App` whose
    // `editor` was absent would refuse every nested write, and a conformance
    // run against it would exercise nothing but the refusal path.
    let lines = transcript("app", &[r#"{"kind":"get","id":"1","path":""}"#]);
    assert_eq!(lines.len(), 1, "one request, one reply: {lines:?}");
    assert_eq!(
        json(&lines[0]),
        serde_json::json!({
            "kind": "value",
            "id": "1",
            "value": {"t": "m", "v": {
                "counter": {"t": "i", "v": "0"},
                "editor": {"t": "m", "v": {
                    "theme": {"t": "s", "v": ""},
                    "zoom": {"t": "f", "v": 0.0},
                }},
                "title": {"t": "s", "v": ""},
            }},
        })
    );
}

#[test]
fn a_write_lands_and_reads_back_at_the_same_path() {
    let lines = transcript(
        "app",
        &[
            r#"{"kind":"set","id":"1","path":"editor.zoom","value":{"t":"f","v":3.25}}"#,
            r#"{"kind":"get","id":"2","path":"editor.zoom"}"#,
        ],
    );
    assert_eq!(json(&lines[0])["kind"], "done");
    assert_eq!(
        json(&lines[1])["value"],
        serde_json::json!({"t": "f", "v": 3.25})
    );
}

#[test]
fn a_push_precedes_the_reply_of_the_request_that_caused_it() {
    // The determinism claim, and the one the fence exists for. Without the
    // fence these two lines race, and the transcript differs between runs on
    // the same machine.
    let lines = transcript(
        "app",
        &[
            r#"{"kind":"subscribe","id":"1","path":"editor.zoom"}"#,
            r#"{"kind":"set","id":"2","path":"editor.zoom","value":{"t":"f","v":9.5}}"#,
        ],
    );
    assert_eq!(lines.len(), 3, "subscribed, notification, done: {lines:?}");
    assert_eq!(json(&lines[0])["kind"], "subscribed");
    assert_eq!(
        json(&lines[1]),
        serde_json::json!({
            "kind": "notification",
            "notification": {
                "subscriber": "0",
                "subscription": "0",
                "patch": {"path": "editor.zoom", "value": {"t": "f", "v": 9.5}},
            },
        }),
        "the push must carry the exact written value, at the written path"
    );
    assert_eq!(json(&lines[2])["kind"], "done");
}

#[test]
fn the_transcript_is_identical_across_runs() {
    // Determinism asserted rather than assumed. Run enough times that a lost
    // race would show: the push/reply ordering is the only nondeterminism
    // this host could have, and it is the one a harness cannot tolerate.
    let requests = [
        r#"{"kind":"subscribe","id":"1","path":"editor"}"#,
        r#"{"kind":"subscribe","id":"2","path":"editor.zoom"}"#,
        r#"{"kind":"set","id":"3","path":"editor.zoom","value":{"t":"f","v":1.5}}"#,
        r#"{"kind":"set","id":"4","path":"counter","value":{"t":"i","v":"5"}}"#,
        r#"{"kind":"unsubscribe","id":"5","subscription":"1"}"#,
        r#"{"kind":"set","id":"6","path":"editor.theme","value":{"t":"s","v":"nord"}}"#,
    ];
    let first = transcript("app", &requests);
    for run in 1..25 {
        assert_eq!(transcript("app", &requests), first, "run {run} differed");
    }
    // And the transcript is the one the ordering rule describes, request by
    // request: two subscriptions; a write both of them overlap, so two pushes
    // then its reply; a write neither overlaps, so a bare reply; the
    // unsubscribe's reply; then a write only the surviving subscription
    // overlaps, so one push then its reply.
    assert_eq!(
        first
            .iter()
            .map(|l| json(l)["kind"].clone())
            .collect::<Vec<_>>(),
        [
            "subscribed",
            "subscribed",
            "notification",
            "notification",
            "done",
            "done",
            "done",
            "notification",
            "done",
        ]
    );
}

#[test]
fn a_guest_cannot_subscribe_as_another_guest() {
    // `handle_text` ignores a `subscriber` on the wire; this is that property
    // observed through the process boundary, where a guest would actually try
    // it. The push must carry this session's own subscriber, not `"999"`.
    let lines = transcript(
        "app",
        &[
            r#"{"kind":"subscribe","id":"1","path":"counter","subscriber":"999"}"#,
            r#"{"kind":"set","id":"2","path":"counter","value":{"t":"i","v":"1"}}"#,
        ],
    );
    assert_eq!(json(&lines[1])["notification"]["subscriber"], "0");
}

#[test]
fn a_refused_line_does_not_end_the_session() {
    // Recovery. One malformed message must not cost a guest its connection —
    // and a host that closed the stream instead would look, from the guest's
    // side, exactly like a hang.
    let lines = transcript(
        "app",
        &[
            "not json at all",
            "",
            r#"{"kind":"nope","id":"1"}"#,
            r#"{"kind":"get","id":"2","path":"counter"}"#,
        ],
    );
    assert_eq!(lines.len(), 4);
    for (n, line) in lines.iter().take(3).enumerate() {
        assert_eq!(json(line)["kind"], "failed", "line {n}: {line}");
    }
    assert_eq!(json(&lines[3])["kind"], "value");
    assert_eq!(json(&lines[3])["id"], "2");
}

#[test]
fn a_line_that_is_not_utf8_is_refused_and_names_the_offset() {
    // `handle_text` takes a `&str`, so this can only be refused here. It must
    // not become a lossy conversion: a request silently mangled into
    // replacement characters would be answered as if it had been read.
    let session = Session::open("app").expect("the fixture should open");
    let mut input =
        Cursor::new(b"{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\xff\xfe\"}\n".to_vec());
    let mut output: Vec<u8> = Vec::new();
    serve(&session, &mut input, &mut output).expect("a cursor cannot fail");
    session.shutdown().expect("the store should stop");

    let text = String::from_utf8(output).expect("the reply is UTF-8 even when the request is not");
    let reply = json(text.trim_end());
    assert_eq!(reply["kind"], "failed");
    assert_eq!(
        reply["id"], "0",
        "text that could not be read cannot have its id recovered"
    );
    assert_eq!(
        reply["message"],
        "the request is not UTF-8: invalid byte at offset 31"
    );
}

#[test]
fn an_overlong_line_is_refused_without_being_echoed() {
    let session = Session::open("app").expect("the fixture should open");
    let mut output: Vec<u8> = Vec::new();
    session
        .serve_overlong(64 * 1024 * 1024, &mut output)
        .expect("a vec cannot fail");
    session.shutdown().expect("the store should stop");

    let text = String::from_utf8(output).expect("UTF-8");
    assert!(
        text.len() < 512,
        "a refusal must stay small however large the line was, got {} bytes",
        text.len()
    );
    let reply = json(text.trim_end());
    assert_eq!(reply["kind"], "failed");
    assert_eq!(reply["id"], "0");
    assert!(
        reply["message"]
            .as_str()
            .is_some_and(|m| m.contains(&MAX_REQUEST_BYTES.to_string())),
        "the refusal must name the limit it enforced, got {reply}"
    );
}

#[test]
fn a_hostile_stream_is_answered_line_by_line_and_never_hangs() {
    // Every shape `crates/duet-protocol/src/text.rs` proves `handle_text`
    // survives, driven through the framing instead of directly — because the
    // framing is the part `handle_text`'s own tests cannot see.
    let mut requests: Vec<String> = vec![
        "[".repeat(200_000),
        r#"{"kind":"get","id":"1","path":"\ud800"}"#.to_string(),
        format!(
            r#"{{"kind":"get","id":"1","path":"{}]"}}"#,
            "a".repeat(100_000)
        ),
        "\u{0007}".to_string(),
        "42".to_string(),
        "{}".to_string(),
        r#"{"kind":"get","id":"007","path":"counter"}"#.to_string(),
    ];
    // A 5,000-deep tagged value, which the depth pre-scan refuses before any
    // of it becomes a tree.
    let mut nested = "null".to_string();
    for _ in 0..5_000 {
        nested = format!(r#"{{"t":"m","v":{{"a":{nested}}}}}"#);
    }
    requests.push(format!(
        r#"{{"kind":"set","id":"1","path":"counter","value":{nested}}}"#
    ));
    // ...and one good request at the end, so the session is proven still alive.
    requests.push(r#"{"kind":"get","id":"9","path":"counter"}"#.to_string());

    let borrowed: Vec<&str> = requests.iter().map(String::as_str).collect();
    let lines = transcript("app", &borrowed);

    assert_eq!(lines.len(), requests.len(), "one reply per line: {lines:?}");
    for (n, line) in lines.iter().take(requests.len() - 1).enumerate() {
        assert_eq!(json(line)["kind"], "failed", "request {n} should fail");
        assert!(
            line.len() < 4096,
            "request {n}'s refusal echoed {} bytes of a request this host does not control",
            line.len()
        );
    }
    assert_eq!(json(&lines[requests.len() - 1])["kind"], "value");
}

#[test]
fn a_session_can_be_opened_for_every_fixture() {
    for name in crate::names() {
        let lines = transcript(name, &[r#"{"kind":"get","id":"1","path":""}"#]);
        assert_eq!(json(&lines[0])["kind"], "value", "{name}");
    }
}

#[test]
fn an_unknown_fixture_refuses_to_open_rather_than_seeding_something_else() {
    assert!(matches!(
        Session::open("nope"),
        Err(SessionError::UnknownFixture { .. })
    ));
}

#[test]
fn the_session_reports_the_fixture_it_was_seeded_from() {
    let session = Session::open("wide").expect("the fixture should open");
    assert_eq!(session.fixture().name, "wide");
    assert_eq!(session.fixture().source, "schema/wide.json");
    session.shutdown().expect("the store should stop");
}

#[test]
fn the_handle_sees_what_the_wire_wrote() {
    // The shared-state claim at its smallest: text arriving on the wire is
    // readable from Rust, through the same store.
    let session = Session::open("app").expect("the fixture should open");
    let mut output: Vec<u8> = Vec::new();
    session
        .serve_line(
            br#"{"kind":"set","id":"1","path":"counter","value":{"t":"i","v":"42"}}"#,
            &mut output,
        )
        .expect("a vec cannot fail");

    let path = Path::parse("counter").expect("a legal path");
    assert_eq!(
        session
            .handle()
            .get(&path)
            .expect("the store should answer"),
        Some(Value::Int(42))
    );
    session.shutdown().expect("the store should stop");
}

#[test]
fn seed_of_matches_what_a_session_actually_starts_with() {
    // Two producers of one fact — the value this host seeds, and the value the
    // corpus states it seeds — must not be able to drift.
    for name in crate::names() {
        let session = Session::open(name).expect("the fixture should open");
        let root = Path::root();
        assert_eq!(
            session
                .handle()
                .get(&root)
                .expect("the store should answer"),
            Some(seed_of(name).expect("the fixture should seed")),
            "{name}"
        );
        session.shutdown().expect("the store should stop");
    }
}

#[test]
fn seed_of_refuses_a_fixture_that_does_not_exist() {
    assert!(matches!(
        seed_of("nope"),
        Err(SessionError::UnknownFixture { .. })
    ));
}

/// A writer that fails on its Nth write.
struct FailsAt {
    remaining: usize,
}

impl Write for FailsAt {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.remaining -= 1;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_closed_output_ends_the_session_rather_than_looping() {
    // A guest that exits mid-conversation closes the pipe. The loop must
    // surface that and stop, not spin writing into a broken descriptor.
    let session = Session::open("app").expect("the fixture should open");
    let mut input = Cursor::new(b"{\"kind\":\"get\",\"id\":\"1\",\"path\":\"\"}\n".repeat(10));
    let mut output = FailsAt { remaining: 0 };
    let error = serve(&session, &mut input, &mut output).expect_err("a broken pipe must surface");
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    session.shutdown().expect("the store should stop");
}
