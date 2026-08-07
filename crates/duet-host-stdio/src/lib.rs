//! A Duet host that speaks the protocol over stdin and stdout.
//!
//! [`duet_protocol::handle_text_with`] is the complete host conversation and
//! needs no window, no Flutter engine and no webview. This crate wraps it in a
//! process so that a **Dart or JavaScript guest can be driven against the real
//! host** across a real process boundary, on any platform CI can run.
//!
//! Until this existed, the generated Dart and TypeScript clients were exercised
//! against fake hosts transcribed by hand from `duet-core`'s write rules. A
//! transcription can be faithfully wrong; this is what makes the difference
//! observable.
//!
//! # Commands
//!
//! Every session registers the three commands in [`commands()`], so a guest's
//! `invoke` has something real to reach. They are hand-written — there is no
//! codegen for commands — and one of them (`bump`) reads and writes the **same
//! store** a guest reads through `get`, which is the property command RPC exists
//! to have and the one a fake host cannot honestly demonstrate.
//!
//! # The framing, exactly
//!
//! **Newline-delimited JSON (NDJSON), UTF-8, in both directions.**
//!
//! - A request is one line: the JSON text, then a single `\n` (`0x0A`).
//! - Every reply is one line: the JSON text [`duet_protocol::handle_text`]
//!   returned, then `\n`.
//! - Every push is one line: the JSON text [`duet_protocol::push_text`]
//!   returned, then `\n`. Pushes travel on the **same** stream as replies, and
//!   a guest tells them apart by the envelope's `kind` — `notification` for a
//!   push, one of `value`/`done`/`subscribed`/`failed` for a reply.
//! - A line is at most [`MAX_REQUEST_BYTES`] bytes, excluding the terminator.
//! - A final line with no terminator is served anyway, then the stream ends.
//! - No banner, no prompt, no trailing summary: every byte on stdout is one of
//!   the three line kinds above.
//!
//! ## Why newlines are safe here, checked rather than assumed
//!
//! Line framing breaks the moment a payload can contain a raw newline, so this
//! is a claim about the encoders and not a convention. All three of them —
//! `serde_json::to_string`, Dart's `jsonEncode` and JavaScript's
//! `JSON.stringify` — escape every character below U+0020 inside a string, so
//! `"a\nb"` travels as the six characters `"a\\nb"` and no `0x0A` byte reaches
//! the wire. `tests/framing.rs` measures that against this host rather than
//! trusting it: it round-trips a string holding **every** code point from
//! U+0000 to U+001F, plus U+007F, U+2028 and U+2029, and asserts the reply and
//! the push carry no `0x0A` and no `0x0D`.
//!
//! U+2028 and U+2029 are in that set for a specific reason: they are line
//! terminators to a *JavaScript source* parser and are **not** escaped by
//! `JSON.stringify`, so they do reach the wire intact. They are not newlines to
//! any of the three line readers involved — Rust's `read_until(b'\n')`, Dart's
//! `LineSplitter` (`\n`, `\r`, `\r\n`) and Node's `readline` (`\n`, `\r\n`) —
//! so framing survives them. `0x0D` is asserted alongside `0x0A` because Dart's
//! `LineSplitter` splits on a lone `\r` too, which is the one place the three
//! readers disagree.
//!
//! ## Why not length-delimited framing
//!
//! A length prefix would remove the dependency on that escaping entirely, and
//! it is the right answer for a transport carrying arbitrary bytes. It is the
//! wrong answer *here* for two reasons. It is not what any real Duet transport
//! does — `wry`'s IPC and Flutter's `BasicMessageChannel` both carry whole
//! messages — so a length-framed harness would be testing a framing no guest
//! ships. And a length prefix is unreadable by hand: `printf '%s\n' '{"kind":…}'
//! | duet-host-stdio app` is a debugging session, while a length-prefixed
//! stream needs a tool to inspect. The escaping claim is one testable
//! property, and it is tested.
//!
//! # Totality
//!
//! stdin is an untrusted input boundary like any other. This host must not
//! panic, must not hang and must not allocate without bound, whatever arrives:
//!
//! - A line longer than [`MAX_REQUEST_BYTES`] is **not** buffered. Bytes past
//!   the limit are counted and dropped as they arrive, and the line is answered
//!   with a `failed` response naming the limit. Peak allocation for a hostile
//!   stream is therefore [`MAX_REQUEST_BYTES`], not the size of the stream.
//! - Bytes that are not UTF-8 are answered with a `failed` response. They never
//!   reach [`duet_protocol::handle_text`], which takes a `&str`.
//! - Everything that *is* UTF-8 goes to [`duet_protocol::handle_text_with`],
//!   which is already total by construction — deep nesting, hostile paths,
//!   megabyte value tags and lone surrogates all become `failed` responses
//!   there, and `crates/duet-protocol/src/text.rs` measures each one. A command
//!   body is the one thing on that path this crate wrote, and its two ways of
//!   breaking a host — panicking, and returning a value too deep to encode —
//!   are caught at `duet_protocol::dispatch_with` rather than here.
//! - A refusal never ends the session. The next line is served normally, so a
//!   guest that sends one bad message does not lose its connection.
//! - Nothing in the non-test source calls `unwrap`, `expect` or `panic!`.
//!
//! # Determinism
//!
//! For one request the output is always: every push that request caused, in
//! order, then the reply. Nothing else can interleave, because this host serves
//! one line at a time and writes both kinds of line from the one thread that
//! reads them.
//!
//! That ordering is not free. [`duet_runtime::StoreHandle::set`] returns as soon
//! as the write has been applied, *before* the core thread hands the batch to
//! the [`duet_runtime::Sink`] — so a naive loop would race its own reply against
//! its own pushes and produce a different transcript on different runs. This
//! host closes that race with a **fence**: after `handle_text` returns it makes
//! one more round trip to the core thread, which cannot be answered until
//! delivery has finished, because the core thread delivers a write's
//! notifications before it accepts the next command. See [`serve::Session`].
//!
//! A real transport does not fence, and a guest must not require that ordering:
//! both guest packages already handle a push arriving before the reply that
//! names its subscription. The fence exists so that a *test harness* reading
//! this stream gets the same bytes every run.
//!
//! # Driving it by hand
//!
//! ```text
//! $ printf '%s\n' \
//!     '{"kind":"subscribe","id":"1","path":"editor.zoom"}' \
//!     '{"kind":"set","id":"2","path":"editor.zoom","value":{"t":"f","v":1.5}}' \
//!   | duet-host-stdio app
//! {"id":"1","kind":"subscribed","snapshot":{"t":"f","v":0.0},"subscription":"0"}
//! {"kind":"notification","notification":{"patch":{"path":"editor.zoom","value":{"t":"f","v":1.5}},"subscriber":"0","subscription":"0"}}
//! {"id":"2","kind":"done"}
//! ```
//!
//! The `subscribed` reply reports the seeded `0.0` this store started at, and
//! the notification arrives before the `done` that answers the write. Both are
//! properties of this host rather than accidents of a run: see [`serve::Session`]
//! for the second, and `tests/framing.rs` for the transcript being byte-stable.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod commands;
pub mod error;
pub mod fixture;
pub mod frame;
pub mod serve;

pub use commands::{BUMP, PING, RAISE, SUBTRACT, commands};
pub use error::SessionError;
pub use fixture::{FIXTURES, Fixture, names};
pub use frame::{Frame, MAX_REQUEST_BYTES, read_frame};
pub use serve::{Session, serve};
