//! Tests for [`super::read_frame`].

use super::*;
use std::io::{Cursor, Read};

/// Reads every frame `input` yields, stopping at [`Frame::Eof`].
fn frames(input: &[u8], max: usize) -> Vec<Frame> {
    let mut reader = Cursor::new(input.to_vec());
    let mut found = Vec::new();
    loop {
        let frame = read_frame(&mut reader, max).expect("a cursor cannot fail");
        let done = frame == Frame::Eof;
        found.push(frame);
        if done {
            return found;
        }
    }
}

#[test]
fn a_terminated_line_yields_its_bytes_without_the_terminator() {
    assert_eq!(
        frames(b"one\ntwo\n", MAX_REQUEST_BYTES),
        vec![
            Frame::Line(b"one".to_vec()),
            Frame::Line(b"two".to_vec()),
            Frame::Eof,
        ]
    );
}

#[test]
fn an_unterminated_final_line_is_still_served() {
    // A guest that wrote a whole request and closed the pipe without a
    // newline gets served rather than ignored. `printf '{…}'` is a debugging
    // session, and one that silently answered nothing would be baffling.
    assert_eq!(
        frames(b"one\ntwo", MAX_REQUEST_BYTES),
        vec![
            Frame::Line(b"one".to_vec()),
            Frame::Line(b"two".to_vec()),
            Frame::Eof,
        ]
    );
}

#[test]
fn an_empty_line_is_a_line_and_not_an_end_of_stream() {
    // The distinction that keeps a doubled newline from ending the session.
    // An empty line becomes an ordinary `failed` response one layer up.
    assert_eq!(
        frames(b"\n\na\n", MAX_REQUEST_BYTES),
        vec![
            Frame::Line(Vec::new()),
            Frame::Line(Vec::new()),
            Frame::Line(b"a".to_vec()),
            Frame::Eof,
        ]
    );
}

#[test]
fn an_empty_stream_is_end_of_stream_immediately() {
    assert_eq!(frames(b"", MAX_REQUEST_BYTES), vec![Frame::Eof]);
}

#[test]
fn a_carriage_return_stays_in_the_line() {
    // Splitting on `\r` too would be guessing. `serde_json` reads a trailing
    // `\r` as the whitespace it is, so a CRLF guest is served; a `\r` in the
    // middle of a line is not a terminator to Rust or to Node, and the JSON
    // parser refuses it as it would any other stray control character.
    assert_eq!(
        frames(b"one\r\ntwo\r\n", MAX_REQUEST_BYTES),
        vec![
            Frame::Line(b"one\r".to_vec()),
            Frame::Line(b"two\r".to_vec()),
            Frame::Eof,
        ]
    );
}

#[test]
fn a_line_at_exactly_the_limit_is_kept_whole() {
    // The boundary, pinned. An off-by-one here refuses a legal request or
    // accepts one byte past the ceiling, and neither shows up anywhere else.
    let line = vec![b'a'; 8];
    let mut input = line.clone();
    input.push(b'\n');
    assert_eq!(frames(&input, 8), vec![Frame::Line(line), Frame::Eof]);
}

#[test]
fn a_line_one_byte_past_the_limit_is_refused_whole() {
    let mut input = vec![b'a'; 9];
    input.push(b'\n');
    assert_eq!(
        frames(&input, 8),
        vec![Frame::Overlong { dropped: 1 }, Frame::Eof]
    );
}

#[test]
fn the_line_after_an_overlong_one_is_served_normally() {
    // Recovery is the whole point of counting rather than aborting: one bad
    // line must not cost a guest its connection.
    let mut input = vec![b'a'; 100];
    input.extend_from_slice(b"\ngood\n");
    assert_eq!(
        frames(&input, 8),
        vec![
            Frame::Overlong { dropped: 92 },
            Frame::Line(b"good".to_vec()),
            Frame::Eof,
        ]
    );
}

#[test]
fn an_overlong_line_that_ends_at_eof_is_still_refused() {
    let input = vec![b'a'; 100];
    assert_eq!(
        frames(&input, 8),
        vec![Frame::Overlong { dropped: 92 }, Frame::Eof]
    );
}

#[test]
fn a_zero_limit_refuses_every_non_empty_line_and_keeps_empty_ones() {
    assert_eq!(
        frames(b"a\n\n", 0),
        vec![
            Frame::Overlong { dropped: 1 },
            Frame::Line(Vec::new()),
            Frame::Eof,
        ]
    );
}

/// A reader that hands out one byte at a time, so `read_frame`'s loop is
/// driven across many `fill_buf` calls rather than one.
struct Trickle {
    bytes: Vec<u8>,
    at: usize,
    held: Vec<u8>,
}

impl Read for Trickle {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.bytes.len() || out.is_empty() {
            return Ok(0);
        }
        out[0] = self.bytes[self.at];
        self.at += 1;
        Ok(1)
    }
}

impl BufRead for Trickle {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.held.is_empty() && self.at < self.bytes.len() {
            self.held.push(self.bytes[self.at]);
            self.at += 1;
        }
        Ok(&self.held)
    }

    fn consume(&mut self, amount: usize) {
        self.held.drain(..amount.min(self.held.len()));
    }
}

#[test]
fn a_line_split_across_many_reads_is_reassembled() {
    // `fill_buf` may return one byte at a time, and a version of this that
    // assumed a whole line per call would silently truncate every request on
    // a slow pipe — which is exactly what a pipe from another process is.
    let mut reader = Trickle {
        bytes: b"hello\nworld\n".to_vec(),
        at: 0,
        held: Vec::new(),
    };
    assert_eq!(
        read_frame(&mut reader, MAX_REQUEST_BYTES).expect("read"),
        Frame::Line(b"hello".to_vec())
    );
    assert_eq!(
        read_frame(&mut reader, MAX_REQUEST_BYTES).expect("read"),
        Frame::Line(b"world".to_vec())
    );
    assert_eq!(
        read_frame(&mut reader, MAX_REQUEST_BYTES).expect("read"),
        Frame::Eof
    );
}

#[test]
fn an_overlong_line_split_across_many_reads_counts_every_dropped_byte() {
    let mut reader = Trickle {
        bytes: b"aaaaaaaaaaaa\n".to_vec(),
        at: 0,
        held: Vec::new(),
    };
    assert_eq!(
        read_frame(&mut reader, 4).expect("read"),
        Frame::Overlong { dropped: 8 }
    );
}

/// A reader that reports `Interrupted` once before yielding anything.
struct InterruptOnce {
    interrupted: bool,
    rest: Cursor<Vec<u8>>,
}

impl Read for InterruptOnce {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.rest.read(out)
    }
}

impl BufRead for InterruptOnce {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
        }
        self.rest.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.rest.consume(amount);
    }
}

#[test]
fn an_interrupted_read_is_retried_rather_than_failing_the_session() {
    // A signal arriving mid-syscall is not an error, and a host that ended a
    // guest's session over one would be maddening to debug.
    let mut reader = InterruptOnce {
        interrupted: false,
        rest: Cursor::new(b"hi\n".to_vec()),
    };
    assert_eq!(
        read_frame(&mut reader, MAX_REQUEST_BYTES).expect("read"),
        Frame::Line(b"hi".to_vec())
    );
}

/// A reader that always fails.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _out: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("broken"))
    }
}

impl BufRead for Broken {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Err(std::io::Error::other("broken"))
    }

    fn consume(&mut self, _amount: usize) {}
}

#[test]
fn a_real_read_error_is_reported_rather_than_looped_on() {
    let error = read_frame(&mut Broken, MAX_REQUEST_BYTES).expect_err("a broken read must surface");
    assert_eq!(error.to_string(), "broken");
}

#[test]
fn hostile_input_costs_the_limit_and_not_the_stream() {
    // The allocation claim, at a scale where a version without the cap would
    // be visibly different: 64 MiB of a single unterminated line, read with a
    // 1 KiB ceiling.
    let input = vec![b'{'; 64 * 1024 * 1024];
    let mut reader = Cursor::new(input);
    assert_eq!(
        read_frame(&mut reader, 1024).expect("read"),
        Frame::Overlong {
            dropped: 64 * 1024 * 1024 - 1024
        }
    );
}
