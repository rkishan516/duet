//! RFC 6455 frame encoding and decoding, as pure functions.
//!
//! Everything in this file is a total function over bytes: no sockets, no
//! timeouts, no state beyond what is passed in. That is deliberate and it is
//! the reason hand-rolling the WebSocket layer is defensible at all — the part
//! most likely to be wrong is the part with no I/O in it, so it is the part
//! that can be tested exhaustively, including against every malformed header a
//! peer could send.
//!
//! # The subset this implements, and what it refuses
//!
//! This client talks to exactly one kind of peer: a Dart VM service on
//! loopback, in a process this tool (or its own child) started. It implements
//! what that conversation needs and **rejects** everything else rather than
//! guessing:
//!
//! | Feature | Here |
//! |---|---|
//! | Text frames, fragmented or not | yes |
//! | Binary frames | decoded, then ignored by the caller — the VM service never sends one |
//! | Ping / Pong / Close | yes |
//! | Client-side masking | yes, mandatory (RFC 6455 §5.3) |
//! | Masked frames *from* the server | **rejected** — §5.1 forbids them |
//! | Reserved bits `RSV1..3` | **rejected** — they mean an extension was negotiated, and none was |
//! | Unknown opcodes | **rejected** |
//! | Fragmented control frames | **rejected** — §5.5 |
//! | Control payloads over 125 bytes | **rejected** — §5.5 |
//! | Payloads over [`MAX_PAYLOAD`] | **rejected** |
//! | `permessage-deflate` and other extensions | never negotiated, so never seen |
//!
//! "Rejected" always means a typed error the caller turns into
//! [`crate::DevError::VmService`]; there is no path here that panics, wraps
//! around, or silently truncates.

use std::fmt;

/// The largest single frame payload this client will accept, in bytes.
///
/// A `getVM` response against a large Flutter app is tens of kilobytes and a
/// `reloadSources` report is smaller still, so 16 MiB is far above anything
/// real. It exists because the 64-bit length field lets a peer *announce*
/// almost 2^63 bytes, and a client that believed it would try to allocate
/// that much before discovering the peer was lying.
pub(crate) const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// The frame opcodes this client understands (RFC 6455 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opcode {
    /// A continuation of the previous fragmented message.
    Continuation,
    /// A UTF-8 text message, or its first fragment.
    Text,
    /// A binary message, or its first fragment.
    Binary,
    /// The peer is closing.
    Close,
    /// A liveness probe that must be answered with [`Opcode::Pong`].
    Ping,
    /// The answer to a [`Opcode::Ping`].
    Pong,
}

impl Opcode {
    /// Decodes the low nibble of a frame's first byte.
    fn from_bits(bits: u8) -> Option<Self> {
        match bits {
            0x0 => Some(Opcode::Continuation),
            0x1 => Some(Opcode::Text),
            0x2 => Some(Opcode::Binary),
            0x8 => Some(Opcode::Close),
            0x9 => Some(Opcode::Ping),
            0xA => Some(Opcode::Pong),
            _ => None,
        }
    }

    /// The wire bits for this opcode.
    fn bits(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xA,
        }
    }

    /// Whether this is a control frame: never fragmented, payload ≤ 125 bytes
    /// (RFC 6455 §5.5).
    pub(crate) fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    /// Whether this is the final fragment of its message.
    pub(crate) fin: bool,
    /// What kind of frame it is.
    pub(crate) opcode: Opcode,
    /// The unmasked payload.
    pub(crate) payload: Vec<u8>,
}

/// Why a peer's bytes were refused.
///
/// Each variant is a distinct RFC rule, so a failure says which one was
/// broken rather than "bad frame".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameError {
    /// One or more of `RSV1`, `RSV2`, `RSV3` was set, but no extension was
    /// negotiated (§5.2).
    ReservedBitsSet(u8),
    /// The opcode is not one of the six defined ones (§5.2).
    UnknownOpcode(u8),
    /// A server frame carried the mask bit, which §5.1 forbids.
    ServerMaskedFrame,
    /// A control frame was fragmented, or carried more than 125 bytes (§5.5).
    BadControlFrame {
        /// Whether the FIN bit was clear.
        fragmented: bool,
        /// The announced payload length.
        length: u64,
    },
    /// The announced payload exceeds [`MAX_PAYLOAD`].
    PayloadTooLarge(u64),
    /// A continuation arrived with no message in progress, or a new
    /// non-control message began while one was (§5.4).
    UnexpectedFragment,
    /// A text message's payload was not valid UTF-8 (§5.6).
    NotUtf8,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::ReservedBitsSet(bits) => write!(
                f,
                "a reserved bit was set (RSV = 0b{bits:03b}) but no extension was negotiated"
            ),
            FrameError::UnknownOpcode(op) => write!(f, "unknown frame opcode 0x{op:x}"),
            FrameError::ServerMaskedFrame => {
                write!(
                    f,
                    "the server sent a masked frame, which RFC 6455 §5.1 forbids"
                )
            }
            FrameError::BadControlFrame { fragmented, length } => write!(
                f,
                "malformed control frame (fragmented={fragmented}, length={length}); \
                 RFC 6455 §5.5 requires FIN and at most 125 bytes"
            ),
            FrameError::PayloadTooLarge(n) => write!(
                f,
                "the peer announced a {n}-byte payload, over this client's {MAX_PAYLOAD}-byte limit"
            ),
            FrameError::UnexpectedFragment => {
                write!(f, "a message fragment arrived out of sequence")
            }
            FrameError::NotUtf8 => write!(f, "a text message was not valid UTF-8"),
        }
    }
}

/// The result of trying to decode one frame from a buffer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decoded {
    /// The buffer does not yet hold a whole frame; read more and retry.
    NeedMore,
    /// A frame, and how many bytes of the buffer it consumed.
    Frame(Frame, usize),
}

/// Decodes the frame at the front of `buf`, if there is a whole one there.
///
/// Never panics and never allocates more than the frame it is about to return:
/// every length is validated against [`MAX_PAYLOAD`] *before* the payload is
/// copied, so a peer announcing a huge frame costs one rejected header rather
/// than an allocation.
pub(crate) fn decode(buf: &[u8]) -> Result<Decoded, FrameError> {
    // Minimum frame: 2 header bytes.
    if buf.len() < 2 {
        return Ok(Decoded::NeedMore);
    }
    let first = buf[0];
    let second = buf[1];

    let fin = first & 0x80 != 0;
    let rsv = (first & 0x70) >> 4;
    if rsv != 0 {
        return Err(FrameError::ReservedBitsSet(rsv));
    }
    let Some(opcode) = Opcode::from_bits(first & 0x0F) else {
        return Err(FrameError::UnknownOpcode(first & 0x0F));
    };

    let masked = second & 0x80 != 0;
    if masked {
        return Err(FrameError::ServerMaskedFrame);
    }

    let short_len = second & 0x7F;
    let (length, header_len) = match short_len {
        126 => {
            if buf.len() < 4 {
                return Ok(Decoded::NeedMore);
            }
            (u64::from(u16::from_be_bytes([buf[2], buf[3]])), 4)
        }
        127 => {
            if buf.len() < 10 {
                return Ok(Decoded::NeedMore);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&buf[2..10]);
            (u64::from_be_bytes(bytes), 10)
        }
        n => (u64::from(n), 2),
    };

    // §5.5: control frames carry the FIN bit and at most 125 bytes. Checked
    // before the size cap so a fragmented 200-byte Ping reports the rule it
    // actually broke.
    if opcode.is_control() && (!fin || length > 125) {
        return Err(FrameError::BadControlFrame {
            fragmented: !fin,
            length,
        });
    }

    // The cap, and with it the `u64 -> usize` narrowing. `MAX_PAYLOAD` fits in
    // a `u32`, so once `length` is under it the conversion is exact on every
    // target this crate supports.
    if length > MAX_PAYLOAD as u64 {
        return Err(FrameError::PayloadTooLarge(length));
    }
    let length = length as usize;

    let total = header_len + length;
    if buf.len() < total {
        return Ok(Decoded::NeedMore);
    }

    Ok(Decoded::Frame(
        Frame {
            fin,
            opcode,
            payload: buf[header_len..total].to_vec(),
        },
        total,
    ))
}

/// Encodes one unfragmented client frame, masked as RFC 6455 §5.3 requires.
///
/// Always sets FIN: this client never fragments what it sends. A
/// `reloadSources` request is a few hundred bytes and the VM service has no
/// trouble with a single frame of that size.
pub(crate) fn encode(opcode: Opcode, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode.bits());

    // The mask bit is always set; the length encoding must be the shortest one
    // that fits, which §5.2 requires and some servers enforce.
    let len = payload.len();
    if len < 126 {
        // `len < 126` fits in the 7-bit field, so this narrowing is exact.
        out.push(0x80 | len as u8);
    } else if len <= u16::MAX as usize {
        out.push(0x80 | 126);
        // Bounded by the branch, so the narrowing is exact.
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }

    out.extend_from_slice(&mask);
    // §5.3: payload[i] XOR mask[i % 4].
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// Reassembles frames into whole messages, enforcing §5.4's fragmentation
/// rules.
///
/// Split from [`decode`] because fragmentation is the one piece of WebSocket
/// framing with state, and mixing it into the byte decoder would make both
/// harder to test. A control frame may arrive *between* the fragments of a
/// message (§5.4 explicitly allows it), which is why this returns control
/// frames straight through without disturbing the buffer.
#[derive(Debug, Default)]
pub(crate) struct Assembler {
    /// The fragments of the message currently in progress, if any, together
    /// with the opcode its first fragment carried.
    partial: Option<(Opcode, Vec<u8>)>,
}

/// What [`Assembler::accept`] produced from one frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// A complete text message.
    Text(String),
    /// A complete binary message. The VM service never sends one; kept so an
    /// unexpected binary frame is reported by the caller rather than silently
    /// treated as text.
    Binary(Vec<u8>),
    /// A control frame, passed straight through.
    Control(Frame),
    /// The frame was a non-final fragment; nothing complete yet.
    Incomplete,
}

impl Assembler {
    /// Folds one frame in, returning a whole message when one completes.
    pub(crate) fn accept(&mut self, frame: Frame) -> Result<Message, FrameError> {
        if frame.opcode.is_control() {
            // §5.4: control frames may be injected in the middle of a
            // fragmented message, so this must not touch `partial`.
            return Ok(Message::Control(frame));
        }

        match frame.opcode {
            Opcode::Continuation => {
                let Some((_, buffered)) = self.partial.as_mut() else {
                    // A continuation with nothing in progress (§5.4).
                    return Err(FrameError::UnexpectedFragment);
                };
                buffered.extend_from_slice(&frame.payload);
            }
            Opcode::Text | Opcode::Binary => {
                if self.partial.is_some() {
                    // A new message starting while one is unfinished (§5.4).
                    return Err(FrameError::UnexpectedFragment);
                }
                self.partial = Some((frame.opcode, frame.payload));
            }
            // Unreachable: control opcodes returned above.
            Opcode::Close | Opcode::Ping | Opcode::Pong => {
                return Ok(Message::Control(frame));
            }
        }

        if !frame.fin {
            return Ok(Message::Incomplete);
        }

        // FIN: hand the assembled message over and reset.
        let Some((opcode, payload)) = self.partial.take() else {
            // Not reachable — every arm above leaves `partial` populated —
            // but expressed as a rule rather than an `expect`, because this
            // crate does not panic.
            return Err(FrameError::UnexpectedFragment);
        };
        match opcode {
            Opcode::Binary => Ok(Message::Binary(payload)),
            // §5.6: a text message must be valid UTF-8, checked once over the
            // whole reassembled message rather than per fragment — a multi-byte
            // character is allowed to straddle a fragment boundary.
            _ => match String::from_utf8(payload) {
                Ok(text) => Ok(Message::Text(text)),
                Err(_) => Err(FrameError::NotUtf8),
            },
        }
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
