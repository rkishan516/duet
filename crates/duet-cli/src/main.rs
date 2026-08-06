//! `duet` — the binary.
//!
//! Everything except the process itself is in `lib.rs`, which is what lets the
//! tool be tested without spawning anything. This file is argv, two streams and
//! an exit code.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let outcome = duet_cli::execute(&arguments);
    emit(&mut io::stdout(), &outcome.out);
    emit(&mut io::stderr(), &outcome.err);
    ExitCode::from(outcome.code)
}

/// Writes `text` to a stream, ignoring a failure to do so.
///
/// A closed stdout — `duet generate ... | head`, or a supervisor that stopped
/// reading — must not change what the process did or what it exits with. The
/// files were already written by the time anything is printed.
fn emit<W: Write>(stream: &mut W, text: &str) {
    if !text.is_empty() {
        let _ = stream.write_all(text.as_bytes());
        let _ = stream.flush();
    }
}
