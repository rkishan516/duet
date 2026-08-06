//! The framing claim, measured: no message this host emits contains a raw
//! line terminator.
//!
//! Newline framing is only safe while that is true, and it is a property of the
//! *encoders* rather than a convention this crate can enforce. `serde_json`,
//! Dart's `jsonEncode` and JavaScript's `JSON.stringify` all escape every
//! character below U+0020 inside a string, so a stored value holding a newline
//! travels as the two characters `\n` and no `0x0A` byte reaches the wire.
//!
//! This file measures that against the real host rather than trusting it,
//! because the failure it prevents is silent and total: one raw newline in one
//! value splits one message into two, and every subsequent reply on the stream
//! answers the wrong request.
//!
//! # What is checked, and what is not
//!
//! `0x0A` **and** `0x0D`. The three line readers involved disagree about
//! exactly one thing: Rust's `read_until(b'\n')` and Node's `readline` split on
//! `\n` and `\r\n`, while Dart's `LineSplitter` also splits on a lone `\r`. So
//! the safe rule is the intersection — neither byte may appear — and that is
//! what these assertions demand.
//!
//! U+2028 and U+2029 are deliberately *in* the payload and deliberately **not**
//! asserted absent. `JSON.stringify` does not escape them, so they genuinely
//! reach the wire; they are line terminators to a JavaScript *source* parser
//! and to no line reader in this system. Including them is what says that was
//! considered rather than overlooked.

use std::io::Cursor;

use duet_core::{Notification, Patch, Path, SubscriberId, SubscriptionId, Value};
use duet_host_stdio::Session;
use duet_protocol::Push;

/// Every character a JSON encoder is obliged to escape, plus the three that
/// look like line terminators to someone.
///
/// U+0000 to U+001F is the whole control range the JSON grammar forbids raw;
/// U+007F is a control character JSON permits raw and which no reader splits
/// on; U+2028 and U+2029 are the JavaScript source terminators.
fn hostile_string() -> String {
    let mut text = String::new();
    for code in 0u32..=0x1F {
        if let Some(c) = char::from_u32(code) {
            text.push(c);
        }
    }
    text.push('\u{7F}');
    text.push('\u{2028}');
    text.push('\u{2029}');
    // And the spellings that would survive a naive escaper: a literal
    // backslash-n, and a quote.
    text.push_str("\\n\"\r\n");
    text
}

/// Fails if `line` holds a byte any of the three line readers would split on.
fn assert_unsplittable(what: &str, line: &str) {
    for (at, byte) in line.bytes().enumerate() {
        assert!(
            byte != b'\n' && byte != b'\r',
            "{what}: byte {byte:#04x} at offset {at} would split this message in two\n{line}"
        );
    }
}

/// Serves `requests` and returns the raw output, un-split.
fn raw_output(requests: &[String]) -> String {
    let session = Session::open("app").expect("the fixture should open");
    let mut input = Cursor::new(requests.join("\n").into_bytes());
    let mut output: Vec<u8> = Vec::new();
    duet_host_stdio::serve(&session, &mut input, &mut output).expect("a cursor cannot fail");
    session.shutdown().expect("the store should stop");
    String::from_utf8(output).expect("this host only ever writes UTF-8")
}

#[test]
fn a_reply_carrying_every_control_character_holds_no_line_terminator() {
    let hostile = hostile_string();
    let encoded = serde_json::to_string(&duet_codec::encode_value(&Value::Str(hostile.clone())))
        .expect("a value always encodes");
    assert_unsplittable("the request this test sends", &encoded);

    let output = raw_output(&[
        format!(r#"{{"kind":"set","id":"1","path":"title","value":{encoded}}}"#),
        r#"{"kind":"get","id":"2","path":"title"}"#.to_string(),
    ]);

    // Two lines, and only two: the whole point is that the payload did not
    // manufacture a third.
    let lines: Vec<&str> = output.trim_end_matches('\n').split('\n').collect();
    assert_eq!(lines.len(), 2, "the payload split the stream: {output:?}");
    for line in &lines {
        assert_unsplittable("a reply", line);
    }

    // ...and the value survived the round trip byte for byte, so the escaping
    // is lossless rather than merely newline-free.
    let reply: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON");
    assert_eq!(
        duet_codec::decode_value(&reply["value"]).expect("the reply must decode"),
        Value::Str(hostile),
        "a string that survives framing must also survive encoding"
    );
}

#[test]
fn a_push_carrying_every_control_character_holds_no_line_terminator() {
    // A push is the other half of the stream and takes a different encoding
    // path — `push_text` rather than `handle_text` — so it needs its own
    // measurement.
    let hostile = hostile_string();
    let encoded = serde_json::to_string(&duet_codec::encode_value(&Value::Str(hostile.clone())))
        .expect("a value always encodes");

    let output = raw_output(&[
        r#"{"kind":"subscribe","id":"1","path":"title"}"#.to_string(),
        format!(r#"{{"kind":"set","id":"2","path":"title","value":{encoded}}}"#),
    ]);

    let lines: Vec<&str> = output.trim_end_matches('\n').split('\n').collect();
    assert_eq!(
        lines.len(),
        3,
        "subscribed, notification, done — and nothing the payload invented: {output:?}"
    );
    for line in &lines {
        assert_unsplittable("a push or reply", line);
    }

    let push: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON");
    assert_eq!(push["kind"], "notification");
    // `Push` is `#[non_exhaustive]`, so this cannot be a `let`-binding.
    match duet_protocol::decode_push(&push).expect("the push must decode") {
        Push::Notification(note) => assert_eq!(note.patch.value, Value::Str(hostile)),
        other => panic!("expected a notification, got {other:?}"),
    }
}

#[test]
fn push_text_holds_no_line_terminator_for_any_control_character_on_its_own() {
    // The same claim one layer down and one character at a time, so a failure
    // names the code point rather than a 100-character blob. Driven through
    // `push_text` directly: it is the encoder, and a loop through a whole
    // session per character would be 34 store round trips for no more signal.
    for code in (0u32..=0x1F).chain([0x7F, 0x2028, 0x2029]) {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        let push = Push::Notification(Notification {
            subscriber: SubscriberId(1),
            subscription: SubscriptionId(1),
            patch: Patch {
                path: Path::parse("title").expect("a legal path"),
                value: Value::Str(c.to_string()),
            },
        });
        assert_unsplittable(
            &format!("a push carrying U+{code:04X}"),
            &duet_protocol::push_text(&push),
        );
    }
}

#[test]
fn a_map_key_holding_a_newline_does_not_split_the_stream() {
    // Map *keys* take a different path through the encoder than string
    // values, and a schema's keys cannot contain one — but `dynamic` and
    // `map<T>` paths hold whatever a guest writes, so a key like this is
    // reachable from a conforming guest.
    let value = Value::Map(
        [("a\nb".to_string(), Value::Int(1))]
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
    );
    let encoded =
        serde_json::to_string(&duet_codec::encode_value(&value)).expect("a value always encodes");

    let output = raw_output(&[
        format!(r#"{{"kind":"set","id":"1","path":"editor","value":{encoded}}}"#),
        r#"{"kind":"get","id":"2","path":"editor"}"#.to_string(),
    ]);
    let lines: Vec<&str> = output.trim_end_matches('\n').split('\n').collect();
    assert_eq!(lines.len(), 2, "a map key split the stream: {output:?}");
    for line in &lines {
        assert_unsplittable("a reply carrying a map key with a newline", line);
    }
}

#[test]
fn the_bytes_codec_never_emits_a_terminator_for_any_byte() {
    // `Value::Bytes` travels as base64, whose alphabet holds neither
    // terminator — but a base64 encoder that wrapped lines at 76 characters,
    // as MIME's does, would break this framing for any payload over 57 bytes.
    // Nothing else in this workspace would notice.
    let all_bytes: Vec<u8> = (0u16..=255).map(|b| b as u8).collect();
    let mut long = Vec::new();
    for _ in 0..16 {
        long.extend_from_slice(&all_bytes);
    }
    let encoded = serde_json::to_string(&duet_codec::encode_value(&Value::Bytes(long.clone())))
        .expect("a value always encodes");
    assert_unsplittable("a 4 KiB base64 payload", &encoded);
    assert_eq!(
        duet_codec::decode_value(&serde_json::from_str(&encoded).expect("valid JSON"))
            .expect("bytes must decode"),
        Value::Bytes(long)
    );
}
