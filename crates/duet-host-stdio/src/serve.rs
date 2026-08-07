//! One session: a seeded store, and the loop that serves lines against it.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{Receiver, Sender, channel};

use duet_command::Commands;
use duet_core::{Notification, Path, SubscriberId, Value};
use duet_protocol::{Push, RequestId, Response};
use duet_runtime::{Runtime, Sink, SinkError, StoreHandle};

use crate::error::SessionError;
use crate::fixture::{self, Fixture};
use crate::frame::{Frame, MAX_REQUEST_BYTES, read_frame};

/// The key the fence reads at.
///
/// A schema key must be a legal path segment, and every one this host is seeded
/// from comes from a `schema/*.json` describing an application's state. A key
/// spelled like this is not one of them — and if it ever were, the fence would
/// still be correct: it never writes, and it discards what it reads. The name
/// is a signpost, not a guard.
const FENCE_KEY: &str = "__duet_host_stdio_fence__";

/// A running host: a seeded store, one subscriber identity, and the queue its
/// notifications land in.
///
/// # The fence, and why a harness needs one
///
/// [`StoreHandle::set`] returns once the write is applied — *before* the core
/// thread hands the batch to the [`Sink`]. So the reply to a `set` and the
/// pushes that `set` caused are produced by two threads with no ordering
/// between them, and a loop that wrote each as it appeared would emit a
/// different transcript on different runs.
///
/// [`Session::serve_line`] closes that with one extra round trip to the core
/// thread after [`duet_protocol::handle_text_with`] returns. The core thread
/// delivers a write's notifications **before** it accepts the next command — see
/// [`Sink::deliver`] — so a reply to that round trip proves delivery already
/// happened, and every push for the request is in the queue by the time the
/// reply is written.
///
/// A real transport does not do this, and no guest may require it: both guest
/// packages already handle a push arriving ahead of the reply that names its
/// subscription. The fence buys a byte-identical transcript for a test harness,
/// nothing more.
pub struct Session {
    /// The fixture this session was seeded from.
    fixture: Fixture,
    /// The core thread. Held so it lives as long as the session.
    runtime: Runtime,
    /// The handle every request is served through.
    handle: StoreHandle,
    /// This session's single guest identity. A guest cannot name another.
    subscriber: SubscriberId,
    /// Where the sink parks batches until the loop drains them.
    notifications: Receiver<Vec<Notification>>,
    /// The path the fence reads at, parsed once.
    fence: Path,
    /// The commands this session's guest may reach.
    ///
    /// Built once per session rather than once per line: a registry is the
    /// authorization boundary, and rebuilding it per request would make "which
    /// commands does this surface have" a question with a per-request answer.
    commands: Commands,
}

impl Session {
    /// Opens a session seeded from the named schema fixture.
    ///
    /// # Errors
    ///
    /// [`SessionError`] if no fixture goes by `name`, or if the embedded schema
    /// no longer parses.
    pub fn open(name: &str) -> Result<Session, SessionError> {
        let (fixture, schema) = fixture::schema(name)?;
        let (tx, notifications) = channel();
        let runtime = Runtime::spawn(duet_schema::seed(&schema), QueueSink { tx });
        let handle = runtime.handle();
        let subscriber = handle.next_subscriber_id();
        Ok(Session {
            fixture,
            runtime,
            handle,
            subscriber,
            notifications,
            // `Path::parse` on a constant this crate owns cannot fail, but
            // saying so with an `expect` would put a panic on a startup path a
            // guest is waiting on. `Path::root()` is the honest fallback: it
            // reads the whole tree, which is slower and just as correct as a
            // fence.
            fence: Path::parse(FENCE_KEY).unwrap_or_else(|_| Path::root()),
            commands: crate::commands::commands(),
        })
    }

    /// The fixture this session was seeded from.
    #[must_use]
    pub fn fixture(&self) -> Fixture {
        self.fixture
    }

    /// The store behind this session, for a test that wants to look at it
    /// without going through the wire.
    #[must_use]
    pub fn handle(&self) -> StoreHandle {
        self.handle.clone()
    }

    /// The commands this session serves, for a test that wants to state what a
    /// guest may reach without going through the wire.
    #[must_use]
    pub fn commands(&self) -> &Commands {
        &self.commands
    }

    /// Serves one line, writing every push it caused and then its reply.
    ///
    /// # Errors
    ///
    /// Whatever `output` returned. Nothing else can fail: the reply itself is
    /// produced by [`duet_protocol::handle_text_with`], which is total — including
    /// against a command body, whose two ways of breaking a host (panicking, and
    /// returning a value too deep to encode) are caught before they reach here.
    pub fn serve_line<W: Write>(&self, line: &[u8], output: &mut W) -> io::Result<()> {
        let reply = match std::str::from_utf8(line) {
            Ok(text) => {
                duet_protocol::handle_text_with(&self.handle, self.subscriber, &self.commands, text)
            }
            Err(e) => refusal(&format!(
                "the request is not UTF-8: invalid byte at offset {}",
                e.valid_up_to()
            )),
        };
        self.fence();
        self.write_pending_pushes(output)?;
        write_line(output, &reply)
    }

    /// Answers a line that was never buffered, so nothing of it is echoed.
    ///
    /// # Errors
    ///
    /// Whatever `output` returned.
    pub fn serve_overlong<W: Write>(&self, dropped: usize, output: &mut W) -> io::Result<()> {
        write_line(
            output,
            &refusal(&format!(
                "the request is longer than the {MAX_REQUEST_BYTES} bytes this host \
                 admits on one line; {dropped} bytes past the limit were discarded"
            )),
        )
    }

    /// Waits until every notification the last request caused has been
    /// delivered. See this type's documentation.
    fn fence(&self) {
        // A read at a key nothing in a schema occupies: one round trip to the
        // core thread, cloning nothing. The result is discarded — the round
        // trip is the whole point — and an error means the core thread is
        // already gone, in which case there is nothing left to wait for.
        let _ = self.handle.get(&self.fence);
    }

    /// Writes every notification currently queued, oldest first.
    fn write_pending_pushes<W: Write>(&self, output: &mut W) -> io::Result<()> {
        for batch in self.notifications.try_iter() {
            for note in batch {
                write_line(output, &duet_protocol::push_text(&Push::Notification(note)))?;
            }
        }
        Ok(())
    }

    /// Stops the core thread.
    ///
    /// # Errors
    ///
    /// [`duet_runtime::RuntimeError`] if the core thread had already gone.
    pub fn shutdown(self) -> Result<(), duet_runtime::RuntimeError> {
        self.runtime.shutdown()
    }
}

/// Serves every line of `input` against `session`, returning at end of input.
///
/// # Errors
///
/// Whatever `output` or `input` returned. A read error ends the session; a
/// malformed *request* does not, and is answered like any other.
pub fn serve<R: BufRead, W: Write>(
    session: &Session,
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    loop {
        match read_frame(input, MAX_REQUEST_BYTES)? {
            Frame::Line(line) => session.serve_line(&line, output)?,
            Frame::Overlong { dropped } => session.serve_overlong(dropped, output)?,
            Frame::Eof => return output.flush(),
        }
        // Flushed per line rather than per session: a guest is blocked on this
        // reply, and a buffered one is a deadlock rather than a slow answer.
        output.flush()?;
    }
}

/// The `failed` response for text this host refused before the protocol saw it.
///
/// Carries [`RequestId::UNCORRELATED`], spelled `"0"`, for the same reason
/// `duet_protocol::text`'s decoder does: reading the id would mean parsing the
/// very bytes being refused, and a reply naming an id the guest never sent
/// would leave that call hanging rather than failing.
fn refusal(message: &str) -> String {
    let response = Response::Failed {
        id: RequestId::UNCORRELATED,
        message: message.to_string(),
    };
    serde_json::to_string(&duet_protocol::encode_response(&response))
        .unwrap_or_else(|_| FALLBACK_REFUSAL.to_string())
}

/// Emitted only if serializing a `Response` itself fails, which cannot happen
/// for the shape [`refusal`] builds. Present so that function can be total
/// without an `expect`. Mirrors `duet_protocol::text`'s own fallback.
const FALLBACK_REFUSAL: &str =
    r#"{"kind":"failed","id":"0","message":"host could not serialize its response"}"#;

/// Writes `text` and the terminator that frames it.
fn write_line<W: Write>(output: &mut W, text: &str) -> io::Result<()> {
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")
}

/// Parks batches for the serve loop to drain.
///
/// Sends the [`Notification`]s themselves rather than their encoded text: the
/// [`Sink`] contract is that `deliver` runs on the core thread and must not
/// serialize, because everything it does is head-of-line latency for every
/// other reader and writer. Encoding happens in [`Session::write_pending_pushes`],
/// after the batch has left that thread.
struct QueueSink {
    tx: Sender<Vec<Notification>>,
}

impl Sink for QueueSink {
    fn deliver(&self, batch: Vec<Notification>) -> Result<(), SinkError> {
        self.tx.send(batch).map_err(|_| SinkError::Closed)
    }
}

/// The seed a session of `name` starts from, for a caller that needs to state
/// the same thing this host was started with.
///
/// # Errors
///
/// [`SessionError`] if no fixture goes by `name`.
pub fn seed_of(name: &str) -> Result<Value, SessionError> {
    let (_, schema) = fixture::schema(name)?;
    Ok(duet_schema::seed(&schema))
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
