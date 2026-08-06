//! A blocking WebSocket client over `TcpStream`, deadline-aware throughout.
//!
//! # The honest cost of not depending on `tungstenite`
//!
//! This is a hand-rolled implementation of a protocol with a real history of
//! implementation bugs, and it should be read with that in mind. What makes
//! the trade acceptable here, specifically:
//!
//! - **The peer is not hostile and not remote.** It is a Dart VM service on
//!   `127.0.0.1`, in a process this tool or its own child just started, in a
//!   debug build. There is no proxy, no TLS, no untrusted network path, and no
//!   attacker-chosen input — the classic WebSocket CVE shapes (masking-key
//!   cache poisoning, `permessage-deflate` decompression bombs, HTTP request
//!   smuggling through a proxy) have no reachable surface.
//! - **The risky part has no I/O in it.** Framing lives in [`crate::frame`] as
//!   pure functions with direct tests over every malformed header this file
//!   could receive. What is left here is a handshake, a read loop and a write
//!   call.
//! - **It refuses rather than guesses.** No extension is ever negotiated, so
//!   any reserved bit, unknown opcode, masked server frame or oversized
//!   payload is an error, not a code path.
//! - **It buys `duet-cli` its dependency count back.** See this crate's
//!   `Cargo.toml`.
//!
//! What this costs, stated plainly: `permessage-deflate` is unsupported (never
//! offered, so never negotiated), there is no TLS (the VM service is
//! `ws://` on loopback), and this code has this crate's tests behind it rather
//! than an ecosystem's. If Duet ever needs to speak WebSocket to something it
//! did not launch itself, this file is not the thing to reach for — take
//! `tungstenite` then, and delete this.
//!
//! # Deadlines
//!
//! Every read is bounded. [`WebSocket::next_text`] takes an absolute deadline
//! and re-derives the socket's read timeout from it on every pass, so a peer
//! that dribbles one byte at a time cannot extend the wait indefinitely —
//! which a per-`read` timeout alone would allow.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::base64;
use crate::error::{DevError, Stage};
use crate::frame::{self, Assembler, Decoded, Message, Opcode};
use crate::sha1::sha1;
use crate::url::VmServiceUrl;

/// The magic string RFC 6455 §1.3 appends to the client key before hashing.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// How much to read from the socket at a time.
const READ_CHUNK: usize = 16 * 1024;

/// The largest handshake response header block this client will read, in
/// bytes. A real one is a few hundred; this bound stops a peer that never
/// sends the terminating blank line from growing the buffer without limit.
const MAX_HANDSHAKE: usize = 64 * 1024;

/// A connected WebSocket.
pub(crate) struct WebSocket {
    stream: TcpStream,
    /// Bytes read from the socket but not yet decoded into frames.
    inbox: Vec<u8>,
    /// Fragmentation state.
    assembler: Assembler,
    /// Counter folded into each masking key so successive frames differ.
    mask_counter: u64,
}

impl WebSocket {
    /// Connects and performs the RFC 6455 opening handshake.
    ///
    /// # Errors
    ///
    /// [`DevError::Io`] if the TCP connect or handshake write failed,
    /// [`DevError::Timeout`] if the server did not answer within `timeout`,
    /// and [`DevError::VmService`] if it answered with something other than a
    /// valid `101 Switching Protocols`.
    pub(crate) fn connect(url: &VmServiceUrl, timeout: Duration) -> Result<Self, DevError> {
        let deadline = Instant::now() + timeout;
        let authority = url.authority();
        let address = authority
            .parse()
            .map_err(|_| DevError::vm(Stage::Connect, format!("{authority} is not an address")))?;
        let stream =
            TcpStream::connect_timeout(&address, timeout).map_err(|source| DevError::Io {
                stage: Stage::Connect,
                doing: "connecting to the Dart VM service",
                source,
            })?;
        // Nagle would add up to 40 ms to every request on a protocol that is
        // entirely small round trips — straight onto the reload latency this
        // whole crate exists to keep low.
        let _ = stream.set_nodelay(true);

        let mut ws = WebSocket {
            stream,
            inbox: Vec::new(),
            assembler: Assembler::default(),
            mask_counter: 0,
        };
        ws.handshake(url, deadline)?;
        Ok(ws)
    }

    /// Sends the client handshake and validates the response.
    fn handshake(&mut self, url: &VmServiceUrl, deadline: Instant) -> Result<(), DevError> {
        let key = base64::encode(&fresh_key());
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {authority}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             \r\n",
            path = url.websocket_path(),
            authority = url.authority(),
        );
        self.stream
            .write_all(request.as_bytes())
            .and_then(|()| self.stream.flush())
            .map_err(|source| DevError::Io {
                stage: Stage::Connect,
                doing: "sending the WebSocket handshake",
                source,
            })?;

        // Read until the header terminator. Anything past it is the first
        // frame bytes and belongs in `inbox`, not thrown away — a fast server
        // can coalesce them into the same TCP segment.
        let header_end = loop {
            if let Some(i) = find_header_end(&self.inbox) {
                break i;
            }
            if self.inbox.len() > MAX_HANDSHAKE {
                return Err(DevError::vm(
                    Stage::Connect,
                    "the handshake response never ended; over 64 KiB of headers",
                ));
            }
            if !self.fill(Stage::Connect, deadline)? {
                return Err(DevError::vm(
                    Stage::Connect,
                    "the connection closed during the handshake",
                ));
            }
        };

        let head = String::from_utf8_lossy(&self.inbox[..header_end]).into_owned();
        self.inbox.drain(..header_end);
        verify_handshake(&head, &key)?;
        Ok(())
    }

    /// Sends one text message.
    ///
    /// # Errors
    ///
    /// [`DevError::Io`] if the write failed.
    pub(crate) fn send_text(&mut self, text: &str) -> Result<(), DevError> {
        self.send(Opcode::Text, text.as_bytes())
    }

    /// Sends one frame, masked.
    fn send(&mut self, opcode: Opcode, payload: &[u8]) -> Result<(), DevError> {
        self.mask_counter = self.mask_counter.wrapping_add(1);
        let bytes = frame::encode(opcode, payload, mask_key(self.mask_counter));
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|source| DevError::Io {
                stage: Stage::Connect,
                doing: "writing to the Dart VM service",
                source,
            })
    }

    /// Returns the next complete text message, answering pings and skipping
    /// anything else, or times out at `deadline`.
    ///
    /// # Errors
    ///
    /// [`DevError::Timeout`] at `deadline`, [`DevError::VmService`] if the
    /// peer closed or broke the protocol, [`DevError::Io`] on a socket error.
    /// `stage` is carried into whichever it produces, so a timeout waiting for
    /// a `reloadSources` reply reports `reload-sources` rather than a generic
    /// transport stage.
    pub(crate) fn next_text(
        &mut self,
        stage: Stage,
        deadline: Instant,
    ) -> Result<String, DevError> {
        loop {
            match frame::decode(&self.inbox) {
                Err(e) => return Err(DevError::vm(stage, e.to_string())),
                Ok(Decoded::NeedMore) => {
                    if !self.fill(stage, deadline)? {
                        return Err(DevError::vm(
                            stage,
                            "the connection closed while waiting for a reply",
                        ));
                    }
                }
                Ok(Decoded::Frame(f, consumed)) => {
                    self.inbox.drain(..consumed);
                    match self.assembler.accept(f) {
                        Err(e) => return Err(DevError::vm(stage, e.to_string())),
                        Ok(Message::Text(text)) => return Ok(text),
                        Ok(Message::Incomplete) => {}
                        Ok(Message::Binary(bytes)) => {
                            // The VM service is a JSON-RPC endpoint and never
                            // sends one. Reported rather than ignored: silently
                            // dropping it would turn a protocol change into a
                            // mysterious timeout.
                            return Err(DevError::vm(
                                stage,
                                format!("expected a text reply, got {} binary bytes", bytes.len()),
                            ));
                        }
                        Ok(Message::Control(control)) => match control.opcode {
                            Opcode::Ping => self.send(Opcode::Pong, &control.payload)?,
                            // A pong we did not ask for is legal (§5.5.3) and
                            // means nothing to us.
                            Opcode::Pong => {}
                            Opcode::Close => {
                                return Err(DevError::vm(
                                    stage,
                                    format!(
                                        "the Dart VM service closed the connection ({})",
                                        describe_close(&control.payload)
                                    ),
                                ));
                            }
                            // Unreachable: `Message::Control` only ever carries
                            // a control opcode.
                            _ => {}
                        },
                    }
                }
            }
        }
    }

    /// Reads one chunk into `inbox`, honouring `deadline`.
    ///
    /// Returns `false` at clean end-of-stream. The socket's read timeout is
    /// re-derived from the deadline on every call, so total wait is bounded
    /// even if the peer keeps the connection alive with dribbled bytes.
    fn fill(&mut self, stage: Stage, deadline: Instant) -> Result<bool, DevError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DevError::Timeout {
                stage,
                after: Duration::ZERO,
            });
        }
        // A zero timeout means "block forever" to the sockets API, so a
        // sub-millisecond remainder must round up rather than down.
        let _ = self
            .stream
            .set_read_timeout(Some(remaining.max(Duration::from_millis(1))));

        let mut chunk = [0u8; READ_CHUNK];
        match self.stream.read(&mut chunk) {
            Ok(0) => Ok(false),
            Ok(n) => {
                self.inbox.extend_from_slice(&chunk[..n]);
                Ok(true)
            }
            Err(e) if is_timeout(&e) => Err(DevError::Timeout {
                stage,
                after: remaining,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(true),
            Err(source) => Err(DevError::Io {
                stage,
                doing: "reading from the Dart VM service",
                source,
            }),
        }
    }
}

impl Drop for WebSocket {
    /// Sends a Close frame so the VM service does not log an abrupt
    /// disconnect. Best effort: a failure here has no recovery and no reader.
    fn drop(&mut self) {
        // 1000 "normal closure", big-endian, per §5.5.1.
        let _ = self.send(Opcode::Close, &1000u16.to_be_bytes());
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Whether an I/O error is a read timeout.
///
/// Platforms disagree: BSDs and macOS report `WouldBlock`, Linux reports
/// `WouldBlock`, Windows reports `TimedOut`. Both are checked so a timeout is
/// never mistaken for a hard failure.
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// The index just past the `\r\n\r\n` that ends an HTTP header block.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Checks a handshake response: `101`, and the right `Sec-WebSocket-Accept`.
///
/// The accept check is what distinguishes a real WebSocket server from an HTTP
/// server, a cache, or a captive portal that answered `101` by accident. It is
/// cheap and it is the entire reason [`crate::sha1`] exists.
fn verify_handshake(head: &str, key: &str) -> Result<(), DevError> {
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default();
    if !status.contains(" 101") {
        return Err(DevError::vm(
            Stage::Connect,
            format!("expected `101 Switching Protocols`, got {status:?}"),
        ));
    }

    let expected = base64::encode(&sha1(format!("{key}{GUID}").as_bytes()));
    let accept = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("sec-websocket-accept")
            .then(|| value.trim())
    });
    match accept {
        Some(found) if found == expected => Ok(()),
        Some(found) => Err(DevError::vm(
            Stage::Connect,
            format!("Sec-WebSocket-Accept was {found:?}, expected {expected:?}"),
        )),
        None => Err(DevError::vm(
            Stage::Connect,
            "the response had no Sec-WebSocket-Accept header",
        )),
    }
}

/// Renders a Close frame's payload for an error message: the status code and
/// any UTF-8 reason the peer gave.
fn describe_close(payload: &[u8]) -> String {
    if payload.len() < 2 {
        return "no status code".to_string();
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = String::from_utf8_lossy(&payload[2..]);
    if reason.is_empty() {
        format!("status {code}")
    } else {
        format!("status {code}: {reason}")
    }
}

/// Sixteen bytes for `Sec-WebSocket-Key`.
///
/// **Not cryptographic, and does not need to be.** §4.1 wants the key
/// unpredictable so that an intermediary cannot be tricked into treating a
/// crafted HTTP response as a valid upgrade. There is no intermediary here —
/// this connects to a loopback port on a process this tool started — and the
/// key's only local job is to make the server's accept hash a value only a
/// real WebSocket server could compute for *this* connection. Clock nanos plus
/// the process id give that.
fn fresh_key() -> [u8; 16] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let pid = u64::from(std::process::id());
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&nanos.to_le_bytes());
    key[8..].copy_from_slice(&(pid ^ nanos.rotate_left(17)).to_le_bytes());
    key
}

/// A four-byte masking key derived from a per-connection counter.
///
/// Same reasoning as [`fresh_key`]: §5.3's unpredictability requirement exists
/// to stop a client being coerced into emitting attacker-chosen bytes through
/// a proxy that might cache them. No proxy, no cache, loopback only. What does
/// matter is that successive frames use *different* keys, which the counter
/// guarantees.
fn mask_key(counter: u64) -> [u8; 4] {
    let mixed = counter
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(31)
        .wrapping_mul(0xBF58_476D_1CE4_E5B9);
    [
        (mixed >> 24) as u8,
        (mixed >> 32) as u8,
        (mixed >> 40) as u8,
        (mixed >> 48) as u8,
    ]
}

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;
