//! Reading one bounded line from an untrusted stream.

use std::io::{self, BufRead};

/// The most bytes one request line may carry, excluding its `\n`.
///
/// One mebibyte. Chosen to be far above anything a conforming guest sends — the
/// largest message either guest package produces is a few hundred bytes, and
/// the wire's own nesting limit caps depth long before this caps width — and
/// far below anything that would matter to a machine running a test suite.
///
/// The number matters less than the fact that there *is* one. Without it,
/// `duet-host-stdio </dev/zero` grows until the machine gives out, and a
/// harness that can be killed by its own fixture is not a harness.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

/// What one read from the input stream produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A complete line, without its terminator.
    ///
    /// Also produced for a final line that ended at EOF with no terminator: a
    /// well-formed request typed without a trailing newline is served, and a
    /// truncated one becomes an ordinary `failed` response.
    Line(Vec<u8>),
    /// A line past [`MAX_REQUEST_BYTES`], discarded as it arrived.
    Overlong {
        /// How many bytes past the limit were dropped. The line's bytes are
        /// **not** retained, so this is the only trace of them.
        dropped: usize,
    },
    /// The stream ended on a line boundary.
    Eof,
}

/// Reads one line from `reader`, keeping at most [`MAX_REQUEST_BYTES`] of it.
///
/// Splits on `\n` (`0x0A`) alone. A `\r` before it is left in the line, where
/// `serde_json` treats it as the trailing whitespace it is — so a guest writing
/// CRLF is served rather than refused, without this function having to guess
/// which of the three line conventions produced it.
///
/// # Bounded by construction
///
/// The accumulator never grows past `max`. Bytes arriving after that are
/// counted and dropped in place, so a stream with no newline in it at all costs
/// `max` bytes of memory however long it runs.
///
/// # Errors
///
/// Whatever `reader` returned. [`io::ErrorKind::Interrupted`] is retried, as
/// every well-behaved reader loop must: it is not a failure, it is a signal
/// that arrived mid-syscall.
pub fn read_frame<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Frame> {
    let mut line = LineBuf::new(max);
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(line.at_eof());
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(at) => {
                line.push(&available[..at]);
                reader.consume(at + 1);
                return Ok(line.finish());
            }
            None => {
                let taken = available.len();
                line.push(available);
                reader.consume(taken);
            }
        }
    }
}

/// One line under construction, with a hard ceiling on what it retains.
struct LineBuf {
    bytes: Vec<u8>,
    dropped: usize,
    max: usize,
}

impl LineBuf {
    fn new(max: usize) -> LineBuf {
        LineBuf {
            bytes: Vec::new(),
            dropped: 0,
            max,
        }
    }

    /// Appends what fits and counts what does not.
    fn push(&mut self, chunk: &[u8]) {
        let room = self.max.saturating_sub(self.bytes.len());
        let taken = room.min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..taken]);
        self.dropped = self.dropped.saturating_add(chunk.len() - taken);
    }

    /// The frame for a line that ended at its terminator.
    fn finish(self) -> Frame {
        if self.dropped > 0 {
            // The retained prefix is dropped with `self`: an overlong line is
            // refused whole, and keeping a megabyte of it to quote back would
            // be the amplification this limit exists to prevent.
            Frame::Overlong {
                dropped: self.dropped,
            }
        } else {
            Frame::Line(self.bytes)
        }
    }

    /// The frame for a line that ended at end of input.
    fn at_eof(self) -> Frame {
        if self.dropped == 0 && self.bytes.is_empty() {
            Frame::Eof
        } else {
            self.finish()
        }
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
