//! The connection: handshake, read loop, control frames, deadlines.
//!
//! Against a real socket and a real (independently written) server — see
//! [`crate::test_server`].

use std::time::{Duration, Instant};

use super::*;
use crate::test_server::{Handshake, TestServer, read_text, write_frame, write_text};

const QUICK: Duration = Duration::from_secs(5);

fn deadline() -> Instant {
    Instant::now() + QUICK
}

#[test]
fn a_text_message_round_trips_over_a_real_socket() {
    let server = TestServer::start(Handshake::Correct, |stream| {
        let Some(request) = read_text(stream) else {
            return;
        };
        write_text(stream, &format!("echo:{request}"));
    });
    let mut socket =
        WebSocket::connect(&server.url(), QUICK).expect("the handshake should succeed");
    socket.send_text("hello").expect("send should succeed");
    assert_eq!(
        socket
            .next_text(Stage::Connect, deadline())
            .expect("a reply should arrive"),
        "echo:hello"
    );
}

#[test]
fn the_handshake_is_rejected_when_the_accept_hash_is_wrong() {
    // This is the check `sha1.rs` exists for. Without it, anything that
    // answers `101` — a proxy, a cached response, an unrelated service —
    // would be treated as a Dart VM service, and the failure would surface
    // much later as unintelligible frames.
    let server = TestServer::start(Handshake::WrongAccept, |_| {});
    let Err(e) = WebSocket::connect(&server.url(), QUICK) else {
        panic!("a wrong accept hash must not be accepted");
    };
    assert_eq!(e.stage(), Stage::Connect);
    assert!(
        e.to_string().contains("Sec-WebSocket-Accept"),
        "the error should name the header: {e}"
    );
}

#[test]
fn the_handshake_is_rejected_when_there_is_no_accept_header() {
    let server = TestServer::start(Handshake::NoAccept, |_| {});
    let Err(e) = WebSocket::connect(&server.url(), QUICK) else {
        panic!("a missing accept header must not be accepted");
    };
    assert!(e.to_string().contains("no Sec-WebSocket-Accept"), "got {e}");
}

#[test]
fn pointing_at_an_ordinary_http_server_says_so() {
    // The most likely real mistake: a mistyped port that happens to hit
    // something else. The message must name what came back.
    let server = TestServer::start(Handshake::NotWebSocket, |_| {});
    let Err(e) = WebSocket::connect(&server.url(), QUICK) else {
        panic!("a 200 response is not a WebSocket upgrade");
    };
    let text = e.to_string();
    assert!(text.contains("101"), "the error should mention 101: {text}");
    assert!(text.contains("200"), "and what arrived instead: {text}");
}

#[test]
fn a_server_that_never_answers_times_out_at_the_connect_stage() {
    // Not hanging forever is the whole point. The deadline is short here
    // because the assertion is that it fires at all.
    let server = TestServer::start(Handshake::Silent, |_| {});
    let started = Instant::now();
    let Err(e) = WebSocket::connect(&server.url(), Duration::from_millis(300)) else {
        panic!("a silent server must not produce a connection");
    };
    assert!(
        matches!(
            e,
            DevError::Timeout {
                stage: Stage::Connect,
                ..
            }
        ),
        "expected a connect timeout, got {e:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it should give up promptly, took {:?}",
        started.elapsed()
    );
}

#[test]
fn connecting_to_a_closed_port_is_an_io_error_not_a_hang() {
    // `free_port` hands back a port nobody is listening on, which is exactly
    // the shape of "the engine failed to start its VM service".
    let port = crate::locate::free_port().expect("a free port should be available");
    let Err(e) = WebSocket::connect(&VmServiceUrl::loopback(port), Duration::from_millis(500))
    else {
        panic!("nothing is listening on that port");
    };
    assert_eq!(e.stage(), Stage::Connect);
}

#[test]
fn a_ping_is_answered_with_a_pong_carrying_the_same_payload() {
    // §5.5.2 requires the same application data back. A server that pings to
    // check liveness will close the connection if this is wrong — which would
    // show up as a reload that works for ten minutes and then stops.
    let server = TestServer::start(Handshake::Correct, |stream| {
        write_frame(stream, true, 0x9, b"are you there");
        // The pong is a control frame; `read_text` skips those, so read the
        // raw bytes to assert on it.
        let mut header = [0u8; 2];
        if std::io::Read::read_exact(stream, &mut header).is_err() {
            return;
        }
        let len = usize::from(header[1] & 0x7F);
        let mut mask = [0u8; 4];
        let _ = std::io::Read::read_exact(stream, &mut mask);
        let mut payload = vec![0u8; len];
        let _ = std::io::Read::read_exact(stream, &mut payload);
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        let matched = header[0] == 0x8A && payload == b"are you there";
        write_text(stream, if matched { "pong-ok" } else { "pong-wrong" });
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    assert_eq!(
        socket
            .next_text(Stage::ReloadSources, deadline())
            .expect("the reply after the ping should arrive"),
        "pong-ok",
        "a Ping must be answered with a Pong carrying the same payload"
    );
}

#[test]
fn an_unsolicited_pong_is_ignored_and_the_real_reply_still_arrives() {
    // §5.5.3 permits it. Treating it as a reply, or as an error, would both be
    // wrong.
    let server = TestServer::start(Handshake::Correct, |stream| {
        write_frame(stream, true, 0xA, b"unsolicited");
        write_text(stream, "the actual reply");
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    assert_eq!(
        socket
            .next_text(Stage::Connect, deadline())
            .expect("the reply should arrive"),
        "the actual reply"
    );
}

#[test]
fn a_close_frame_is_reported_with_its_status_and_reason() {
    // The Dart VM service closes when its isolate goes away. A driver that
    // reported "timed out" for that would send the developer looking in the
    // wrong place entirely.
    let server = TestServer::start(Handshake::Correct, |stream| {
        let mut payload = 1001u16.to_be_bytes().to_vec();
        payload.extend_from_slice(b"going away");
        write_frame(stream, true, 0x8, &payload);
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let Err(e) = socket.next_text(Stage::ReloadSources, deadline()) else {
        panic!("a Close frame should not produce a message");
    };
    let text = e.to_string();
    assert_eq!(
        e.stage(),
        Stage::ReloadSources,
        "the caller's stage is kept"
    );
    assert!(
        text.contains("1001"),
        "the status code belongs in it: {text}"
    );
    assert!(text.contains("going away"), "and the reason: {text}");
}

#[test]
fn a_connection_that_closes_without_a_close_frame_is_reported() {
    let server = TestServer::start(Handshake::Correct, |_| {
        // Return immediately; the stream is dropped and the socket closes.
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let Err(e) = socket.next_text(Stage::FindIsolate, deadline()) else {
        panic!("a closed connection should not produce a message");
    };
    assert_eq!(e.stage(), Stage::FindIsolate);
    assert!(e.to_string().contains("closed"), "got {e}");
}

#[test]
fn a_reply_that_never_comes_times_out_at_the_callers_stage() {
    // The single most important behaviour in this crate: a wedged Dart VM must
    // produce "timed out at reload-sources", not silence.
    let server = TestServer::start(Handshake::Correct, |stream| {
        // Hold the connection open, saying nothing.
        std::thread::sleep(Duration::from_secs(10));
        let _ = stream;
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let Err(e) = socket.next_text(
        Stage::ReloadSources,
        Instant::now() + Duration::from_millis(200),
    ) else {
        panic!("a silent server should not produce a message");
    };
    assert!(
        matches!(
            e,
            DevError::Timeout {
                stage: Stage::ReloadSources,
                ..
            }
        ),
        "expected a reload-sources timeout, got {e:?}"
    );
}

#[test]
fn an_already_expired_deadline_fails_immediately_rather_than_reading_once() {
    let server = TestServer::start(Handshake::Correct, |stream| {
        std::thread::sleep(Duration::from_secs(5));
        let _ = stream;
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let started = Instant::now();
    let Err(e) = socket.next_text(Stage::Observe, Instant::now() - Duration::from_secs(1)) else {
        panic!("an expired deadline should not read");
    };
    assert!(matches!(e, DevError::Timeout { .. }), "got {e:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "it should not have waited: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_fragmented_reply_split_across_tcp_writes_reassembles() {
    // Real `getVM` replies are large enough to fragment, and the fragments
    // arrive in separate reads. This is the case the pure codec cannot cover
    // because it never sees a socket.
    let server = TestServer::start(Handshake::Correct, |stream| {
        write_frame(stream, false, 0x1, b"{\"a\":1,");
        std::thread::sleep(Duration::from_millis(20));
        write_frame(stream, false, 0x0, b"\"b\":2,");
        std::thread::sleep(Duration::from_millis(20));
        write_frame(stream, true, 0x0, b"\"c\":3}");
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    assert_eq!(
        socket
            .next_text(Stage::Connect, deadline())
            .expect("the reassembled message should arrive"),
        "{\"a\":1,\"b\":2,\"c\":3}"
    );
}

#[test]
fn a_large_message_survives_the_socket() {
    // Exercises the 16-bit and 64-bit length paths against real segmentation,
    // where a header split across two reads is the failure mode.
    let big = "x".repeat(200_000);
    let expected = big.clone();
    let server = TestServer::start(Handshake::Correct, move |stream| {
        write_text(stream, &big);
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let got = socket
        .next_text(Stage::Connect, Instant::now() + Duration::from_secs(20))
        .expect("a large message should arrive");
    assert_eq!(got.len(), expected.len());
    assert_eq!(got, expected);
}

#[test]
fn a_binary_reply_is_reported_rather_than_silently_dropped() {
    let server = TestServer::start(Handshake::Correct, |stream| {
        write_frame(stream, true, 0x2, &[0u8, 1, 2, 3]);
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let Err(e) = socket.next_text(Stage::FindIsolate, deadline()) else {
        panic!("a binary frame is not a text reply");
    };
    assert!(e.to_string().contains("binary"), "got {e}");
}

#[test]
fn a_protocol_violation_from_the_server_is_reported_at_the_callers_stage() {
    // A reserved bit set means an extension was negotiated that never was.
    let server = TestServer::start(Handshake::Correct, |stream| {
        let _ = std::io::Write::write_all(stream, &[0xC1, 0x02, b'h', b'i']);
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    let Err(e) = socket.next_text(Stage::Reassemble, deadline()) else {
        panic!("a reserved bit must be refused");
    };
    assert_eq!(e.stage(), Stage::Reassemble);
    assert!(e.to_string().contains("reserved"), "got {e}");
}

#[test]
fn several_requests_and_replies_share_one_connection() {
    // Every reload after the first reuses the socket, so a client that only
    // worked once would fail on the second edit.
    let server = TestServer::start(Handshake::Correct, |stream| {
        for _ in 0..3 {
            let Some(request) = read_text(stream) else {
                return;
            };
            write_text(stream, &format!("re:{request}"));
        }
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    for n in 0..3 {
        socket.send_text(&format!("call{n}")).expect("send");
        assert_eq!(
            socket
                .next_text(Stage::Connect, deadline())
                .expect("a reply"),
            format!("re:call{n}")
        );
    }
}

#[test]
fn frame_bytes_arriving_with_the_handshake_response_are_not_lost() {
    // A fast server writes the 101 and the first frame into the same segment.
    // A handshake that discarded everything past the header terminator would
    // drop that frame, and the first reload would time out for no visible
    // reason.
    let server = TestServer::start(Handshake::Correct, |stream| {
        write_text(stream, "arrived with the handshake");
    });
    let mut socket = WebSocket::connect(&server.url(), QUICK).expect("handshake");
    assert_eq!(
        socket
            .next_text(Stage::Connect, deadline())
            .expect("the coalesced frame should still be there"),
        "arrived with the handshake"
    );
}
