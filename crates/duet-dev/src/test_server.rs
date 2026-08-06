//! A real WebSocket server, for tests only.
//!
//! The framing codec is tested exhaustively as pure functions in
//! `frame_tests.rs`. What that cannot reach is everything *around* it: the
//! opening handshake, the read loop's deadline handling, ping/pong, close, and
//! JSON-RPC correlation over an actual socket with actual TCP segmentation.
//!
//! Mocking that would only test the mock. So this is a genuine server on a
//! genuine loopback socket — small enough to be obviously correct, and
//! deliberately written against the RFC rather than against
//! [`crate::ws`], so the two are independent implementations that have to
//! agree. It is the same trick `duet-codegen` uses against `duet-schema`'s
//! hand-rolled writer.
//!
//! It also makes the *hostile* cases reachable: a server that answers 200
//! instead of 101, one that returns the wrong accept hash, one that never
//! replies at all. Those are the paths a developer hits when they point the
//! tool at the wrong port, and they have no other cover.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;

use crate::base64;
use crate::sha1::sha1;
use crate::url::VmServiceUrl;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A one-connection server on a loopback port.
pub(crate) struct TestServer {
    address: SocketAddr,
}

/// How the server should answer the opening handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Handshake {
    /// A correct `101` with the right accept hash.
    Correct,
    /// `101`, but the accept hash is wrong — a peer that is not really a
    /// WebSocket server, or a proxy replaying a cached response.
    WrongAccept,
    /// `101` with no accept header at all.
    NoAccept,
    /// A plain HTTP response, as an ordinary web server would give.
    NotWebSocket,
    /// Accept the TCP connection and then say nothing, forever.
    Silent,
}

impl TestServer {
    /// Starts a server that completes `handshake`, then hands the stream to
    /// `serve` on its own thread.
    pub(crate) fn start(
        handshake: Handshake,
        serve: impl FnOnce(&mut TcpStream) + Send + 'static,
    ) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a test server should bind");
        let address = listener
            .local_addr()
            .expect("a bound listener should have an address");
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            if handshake == Handshake::Silent {
                // Hold the connection open so the client sees a stall rather
                // than a reset, which is what a wedged server looks like.
                thread::sleep(std::time::Duration::from_secs(30));
                return;
            }
            let Some(key) = read_handshake_key(&mut stream) else {
                return;
            };
            let response = match handshake {
                Handshake::Correct => format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                     Connection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                    accept_for(&key)
                ),
                Handshake::WrongAccept => "HTTP/1.1 101 Switching Protocols\r\n\
                     Upgrade: websocket\r\nSec-WebSocket-Accept: bm90IHRoZSByaWdodCBoYXNo\r\n\r\n"
                    .to_string(),
                Handshake::NoAccept => {
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_string()
                }
                Handshake::NotWebSocket => {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi".to_string()
                }
                Handshake::Silent => unreachable!("handled above"),
            };
            if stream.write_all(response.as_bytes()).is_err() {
                return;
            }
            let _ = stream.flush();
            if handshake == Handshake::Correct {
                serve(&mut stream);
            }
        });
        TestServer { address }
    }

    /// The URL a client should connect to.
    pub(crate) fn url(&self) -> VmServiceUrl {
        VmServiceUrl::loopback(self.address.port())
    }
}

/// Reads the client's request and returns its `Sec-WebSocket-Key`.
fn read_handshake_key(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 8192 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("sec-websocket-key")
            .then(|| value.trim().to_string())
    })
}

/// The `Sec-WebSocket-Accept` value for a client key.
fn accept_for(key: &str) -> String {
    base64::encode(&sha1(format!("{key}{GUID}").as_bytes()))
}

/// Writes one unmasked server frame.
pub(crate) fn write_frame(stream: &mut TcpStream, fin: bool, opcode: u8, payload: &[u8]) {
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
    let _ = stream.write_all(&out);
    let _ = stream.flush();
}

/// Writes a complete text message.
pub(crate) fn write_text(stream: &mut TcpStream, text: &str) {
    write_frame(stream, true, 0x1, text.as_bytes());
}

/// Reads one whole client message, unmasking it.
///
/// Deliberately a second, independent implementation of frame parsing: if this
/// and [`crate::frame`] ever disagree, one of them is wrong and these tests
/// find out.
pub(crate) fn read_text(stream: &mut TcpStream) -> Option<String> {
    let mut assembled = Vec::new();
    loop {
        let mut header = [0u8; 2];
        stream.read_exact(&mut header).ok()?;
        let fin = header[0] & 0x80 != 0;
        let opcode = header[0] & 0x0F;
        let masked = header[1] & 0x80 != 0;
        let len = match header[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                stream.read_exact(&mut b).ok()?;
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                stream.read_exact(&mut b).ok()?;
                u64::from_be_bytes(b) as usize
            }
            n => n as usize,
        };
        let mut mask = [0u8; 4];
        if masked {
            stream.read_exact(&mut mask).ok()?;
        }
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).ok()?;
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        match opcode {
            // A Close from the client ends the conversation.
            0x8 => return None,
            // Control frames carry no message payload.
            0x9 | 0xA => continue,
            _ => assembled.extend_from_slice(&payload),
        }
        if fin {
            return String::from_utf8(assembled).ok();
        }
    }
}
