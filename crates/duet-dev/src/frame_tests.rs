//! Frame encoding, decoding, and every malformed header a peer could send.
//!
//! This is the file that makes hand-rolling the WebSocket layer defensible, so
//! it goes past round trips into the rules the RFC states and the shapes a
//! buggy or hostile peer produces.

use super::*;

/// Wraps `payload` as an unmasked server frame, the way a real server would.
fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![if fin { 0x80 | opcode } else { opcode }];
    let len = payload.len();
    if len < 126 {
        out.push(len as u8);
    } else if len <= u16::MAX as usize {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn decode_one(bytes: &[u8]) -> Frame {
    match decode(bytes) {
        Ok(Decoded::Frame(frame, consumed)) => {
            assert_eq!(consumed, bytes.len(), "the whole buffer should be consumed");
            frame
        }
        other => panic!("expected one whole frame, got {other:?}"),
    }
}

#[test]
fn a_client_frame_round_trips_through_the_server_side_decoder() {
    // `encode` masks (clients must) and `decode` rejects masked frames (only
    // servers may send unmasked), so this cannot be a direct round trip.
    // Unmasking by hand here is the point: it checks the mask was applied at
    // all, and applied with the right index arithmetic.
    let mask = [0xDE, 0xAD, 0xBE, 0xEF];
    let payload = b"{\"jsonrpc\":\"2.0\",\"id\":1}";
    let encoded = encode(Opcode::Text, payload, mask);

    assert_eq!(encoded[0], 0x81, "FIN plus the text opcode");
    assert_eq!(encoded[1] & 0x80, 0x80, "the mask bit must be set");
    assert_eq!(
        usize::from(encoded[1] & 0x7F),
        payload.len(),
        "a short payload uses the 7-bit length"
    );
    assert_eq!(&encoded[2..6], &mask, "the mask key follows the length");

    let unmasked: Vec<u8> = encoded[6..]
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ mask[i % 4])
        .collect();
    assert_eq!(unmasked, payload, "masking must be reversible");
}

#[test]
fn the_three_length_encodings_are_each_the_shortest_that_fits() {
    // §5.2 requires the minimal encoding, and some servers enforce it. The
    // boundaries are 125/126 and 65535/65536; getting either off by one
    // produces frames a strict server closes the connection over.
    let cases: [(usize, u8, usize); 5] = [
        // (payload len, expected length byte without the mask bit, header len)
        (0, 0, 6),
        (125, 125, 6),
        (126, 126, 8),
        (65535, 126, 8),
        (65536, 127, 14),
    ];
    for (len, want_byte, want_header) in cases {
        let encoded = encode(Opcode::Text, &vec![b'x'; len], [1, 2, 3, 4]);
        assert_eq!(
            encoded[1] & 0x7F,
            want_byte,
            "a {len}-byte payload should use length byte {want_byte}"
        );
        assert_eq!(
            encoded.len(),
            want_header + len,
            "a {len}-byte payload should have a {want_header}-byte header"
        );
    }
}

#[test]
fn decoding_the_three_length_encodings() {
    for len in [0usize, 1, 125, 126, 300, 70_000] {
        let payload = vec![b'z'; len];
        let frame = decode_one(&server_frame(true, 0x1, &payload));
        assert_eq!(frame.opcode, Opcode::Text);
        assert!(frame.fin);
        assert_eq!(frame.payload, payload, "a {len}-byte payload round-tripped");
    }
}

#[test]
fn a_partial_frame_asks_for_more_rather_than_guessing() {
    // Every prefix of a real frame must report NeedMore. A decoder that read
    // past the buffer here would be a panic in production the first time a TCP
    // segment split a header.
    let whole = server_frame(true, 0x1, b"hello world, this is a longer payload");
    for cut in 0..whole.len() {
        assert_eq!(
            decode(&whole[..cut]),
            Ok(Decoded::NeedMore),
            "a {cut}-byte prefix is not a whole frame"
        );
    }
    assert!(matches!(decode(&whole), Ok(Decoded::Frame(_, _))));
}

#[test]
fn a_partial_extended_length_header_asks_for_more() {
    // The 2- and 8-byte extended lengths are read after the first two bytes,
    // so a buffer holding only part of them is the specific case that would
    // index out of bounds.
    assert_eq!(decode(&[0x81, 126, 0x00]), Ok(Decoded::NeedMore));
    assert_eq!(
        decode(&[0x81, 127, 0, 0, 0, 0, 0, 0, 1]),
        Ok(Decoded::NeedMore)
    );
}

#[test]
fn trailing_bytes_after_a_frame_are_left_in_the_buffer() {
    // The socket reader coalesces reads, so two frames routinely arrive
    // together. `consumed` is what stops the second one being dropped.
    let mut bytes = server_frame(true, 0x1, b"first");
    bytes.extend_from_slice(&server_frame(true, 0x1, b"second"));
    let Ok(Decoded::Frame(frame, consumed)) = decode(&bytes) else {
        panic!("the first frame should decode");
    };
    assert_eq!(frame.payload, b"first");
    let Ok(Decoded::Frame(second, _)) = decode(&bytes[consumed..]) else {
        panic!("the second frame should decode from the remainder");
    };
    assert_eq!(second.payload, b"second");
}

#[test]
fn reserved_bits_are_refused_because_no_extension_was_negotiated() {
    // RSV1 is what `permessage-deflate` sets. This client never offers it, so
    // a peer setting it means the payload is compressed and reading it as text
    // would produce garbage rather than an error.
    for (bits, byte) in [(0b100, 0xC1u8), (0b010, 0xA1), (0b001, 0x91)] {
        assert_eq!(
            decode(&[byte, 0x00]),
            Err(FrameError::ReservedBitsSet(bits)),
            "RSV bits {bits:03b} must be refused"
        );
    }
}

#[test]
fn unknown_opcodes_are_refused() {
    // 0x3..0x7 and 0xB..0xF are reserved. Treating one as text would misread
    // whatever a future protocol version puts there.
    for opcode in [0x3u8, 0x7, 0xB, 0xF] {
        assert_eq!(
            decode(&[0x80 | opcode, 0x00]),
            Err(FrameError::UnknownOpcode(opcode)),
            "opcode 0x{opcode:x} must be refused"
        );
    }
}

#[test]
fn a_masked_server_frame_is_refused() {
    // §5.1: a server must not mask. A client that unmasked anyway would be
    // hiding a peer that is not the server it thinks it is.
    assert_eq!(
        decode(&[0x81, 0x80, 1, 2, 3, 4]),
        Err(FrameError::ServerMaskedFrame)
    );
}

#[test]
fn control_frames_must_be_final_and_short() {
    // §5.5. Both halves, and the size check must fire below the general
    // payload cap so the reported rule is the specific one.
    assert_eq!(
        decode(&[0x09, 0x00]),
        Err(FrameError::BadControlFrame {
            fragmented: true,
            length: 0
        }),
        "a fragmented Ping is malformed"
    );
    let long_ping = server_frame(true, 0x9, &[0u8; 126]);
    assert_eq!(
        decode(&long_ping),
        Err(FrameError::BadControlFrame {
            fragmented: false,
            length: 126
        }),
        "a 126-byte Ping is malformed"
    );
    // Exactly 125 is legal.
    let ok_ping = server_frame(true, 0x9, &[0u8; 125]);
    assert_eq!(decode_one(&ok_ping).opcode, Opcode::Ping);
}

#[test]
fn an_absurd_announced_length_is_refused_before_anything_is_allocated() {
    // The whole reason for the cap: a peer can announce almost 2^63 bytes in
    // ten header bytes. This must be an error, not an allocation.
    let mut header = vec![0x81, 127];
    header.extend_from_slice(&(u64::MAX / 2).to_be_bytes());
    assert_eq!(
        decode(&header),
        Err(FrameError::PayloadTooLarge(u64::MAX / 2))
    );

    // One byte over the cap, with no payload present at all — proving the
    // check happens on the header rather than after reading.
    let mut just_over = vec![0x81, 127];
    just_over.extend_from_slice(&(MAX_PAYLOAD as u64 + 1).to_be_bytes());
    assert_eq!(
        decode(&just_over),
        Err(FrameError::PayloadTooLarge(MAX_PAYLOAD as u64 + 1))
    );
}

#[test]
fn a_fragmented_text_message_reassembles() {
    // The VM service fragments large `getVM` replies. Splitting a multi-byte
    // character across the boundary is the case that breaks a per-fragment
    // UTF-8 check — "café" split mid-`é`.
    let mut assembler = Assembler::default();
    let text = "café ☕";
    let bytes = text.as_bytes();
    let split = 4; // lands inside the two-byte 'é'
    assert!(
        !text.is_char_boundary(split),
        "this test is only meaningful if the split is mid-character"
    );

    let first = decode_one(&server_frame(false, 0x1, &bytes[..split]));
    assert_eq!(assembler.accept(first), Ok(Message::Incomplete));
    let rest = decode_one(&server_frame(true, 0x0, &bytes[split..]));
    assert_eq!(
        assembler.accept(rest),
        Ok(Message::Text(text.to_string())),
        "UTF-8 must be validated over the whole message, not per fragment"
    );
}

#[test]
fn a_control_frame_between_fragments_does_not_disturb_them() {
    // §5.4 explicitly permits this, and a Ping arriving mid-reply is exactly
    // what a slow reload produces. An assembler that folded the Ping into the
    // message would corrupt it.
    let mut assembler = Assembler::default();
    let first = decode_one(&server_frame(false, 0x1, b"{\"jsonrpc\":"));
    assert_eq!(assembler.accept(first), Ok(Message::Incomplete));

    let ping = decode_one(&server_frame(true, 0x9, b"hi"));
    match assembler.accept(ping) {
        Ok(Message::Control(f)) => assert_eq!(f.opcode, Opcode::Ping),
        other => panic!("a Ping should pass straight through, got {other:?}"),
    }

    let rest = decode_one(&server_frame(true, 0x0, b"\"2.0\"}"));
    assert_eq!(
        assembler.accept(rest),
        Ok(Message::Text("{\"jsonrpc\":\"2.0\"}".to_string()))
    );
}

#[test]
fn out_of_sequence_fragments_are_refused() {
    // A continuation with nothing in progress, and a new message while one is
    // unfinished. Both are §5.4 violations and both would silently corrupt a
    // reply if accepted.
    let mut assembler = Assembler::default();
    let orphan = decode_one(&server_frame(true, 0x0, b"nothing started this"));
    assert_eq!(
        assembler.accept(orphan),
        Err(FrameError::UnexpectedFragment)
    );

    let mut assembler = Assembler::default();
    let start = decode_one(&server_frame(false, 0x1, b"one"));
    assert_eq!(assembler.accept(start), Ok(Message::Incomplete));
    let interloper = decode_one(&server_frame(false, 0x1, b"two"));
    assert_eq!(
        assembler.accept(interloper),
        Err(FrameError::UnexpectedFragment)
    );
}

#[test]
fn invalid_utf8_in_a_text_message_is_refused() {
    // §5.6. Lossy decoding would hand JSON parsing a replacement character
    // and produce a confusing parse error instead of a clear transport one.
    let mut assembler = Assembler::default();
    let bad = decode_one(&server_frame(true, 0x1, &[0xFF, 0xFE]));
    assert_eq!(assembler.accept(bad), Err(FrameError::NotUtf8));
}

#[test]
fn a_binary_message_stays_binary() {
    // The VM service never sends one; if it ever did, the caller reports it
    // rather than this layer pretending it was text.
    let mut assembler = Assembler::default();
    let frame = decode_one(&server_frame(true, 0x2, &[0, 1, 2, 255]));
    assert_eq!(
        assembler.accept(frame),
        Ok(Message::Binary(vec![0, 1, 2, 255]))
    );
}

#[test]
fn an_assembler_can_be_reused_after_a_complete_message() {
    // The socket reads many replies over one connection, so a state machine
    // that only worked once would fail on the second reload rather than the
    // first — the worst kind of bug to find.
    let mut assembler = Assembler::default();
    for expected in ["first", "second", "third"] {
        let frame = decode_one(&server_frame(true, 0x1, expected.as_bytes()));
        assert_eq!(
            assembler.accept(frame),
            Ok(Message::Text(expected.to_string()))
        );
    }
}

#[test]
fn every_error_says_which_rule_was_broken() {
    // These strings end up inside a `DevError::VmService` a developer reads.
    // "bad frame" would be useless; each of these names the rule.
    let cases = [
        FrameError::ReservedBitsSet(0b100),
        FrameError::UnknownOpcode(0xB),
        FrameError::ServerMaskedFrame,
        FrameError::BadControlFrame {
            fragmented: true,
            length: 200,
        },
        FrameError::PayloadTooLarge(1 << 40),
        FrameError::UnexpectedFragment,
        FrameError::NotUtf8,
    ];
    for case in cases {
        let text = case.to_string();
        assert!(
            text.len() > 12,
            "{case:?} renders as {text:?}, which says too little"
        );
    }
}

#[test]
fn control_opcodes_are_classified_correctly() {
    // `is_control` gates both the §5.5 check and the assembler's pass-through,
    // so a misclassification would break fragmentation and validation at once.
    for opcode in [Opcode::Close, Opcode::Ping, Opcode::Pong] {
        assert!(opcode.is_control(), "{opcode:?} is a control frame");
    }
    for opcode in [Opcode::Continuation, Opcode::Text, Opcode::Binary] {
        assert!(!opcode.is_control(), "{opcode:?} is a data frame");
    }
}
