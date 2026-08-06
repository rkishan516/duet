//! `duet-host-stdio <schema>` — a Duet host on stdin and stdout.
//!
//! See the crate documentation in `lib.rs` for the framing, the totality
//! guarantees and the determinism argument. This file is argv and exit codes.

use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use duet_host_stdio::{Session, names, serve};

/// argv was wrong. Distinct from a failure of the session itself so a script
/// can tell "you called this incorrectly" from "it ran and stopped".
const EXIT_USAGE: u8 = 2;

/// The session could not be opened, or the stream failed mid-run.
const EXIT_FAILED: u8 = 1;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(name) = one_argument(&arguments) else {
        return usage();
    };

    let session = match Session::open(name) {
        Ok(session) => session,
        Err(e) => return fail(&e.to_string()),
    };

    // Locked once rather than per write: `println!` re-locks stdout on every
    // call, and this loop writes a line per push as well as per reply.
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = BufWriter::new(io::stdout().lock());

    let outcome = serve(&session, &mut input, &mut output);
    // Shut the core thread down before reporting, so the process does not
    // outlive its own store while writing a diagnostic.
    let stopped = session.shutdown();

    match (outcome, stopped) {
        // A broken pipe is the guest closing the connection, which is how a
        // session ends normally when the guest exits first. Reporting it as a
        // failure would make every clean disconnect look like a crash.
        (Err(e), _) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        (Err(e), _) => fail(&format!("the session ended: {e}")),
        (Ok(()), Err(e)) => fail(&format!("the store did not stop cleanly: {e}")),
        (Ok(()), Ok(())) => ExitCode::SUCCESS,
    }
}

/// The single positional argument, or `None` if argv is not exactly one.
fn one_argument(arguments: &[String]) -> Option<&str> {
    match arguments {
        [only] if !only.starts_with('-') => Some(only.as_str()),
        _ => None,
    }
}

/// Prints how to call this and exits.
fn usage() -> ExitCode {
    report(&format!(
        "usage: duet-host-stdio <schema>\n\
         \n\
         Serves the Duet protocol as newline-delimited JSON on stdin and stdout,\n\
         against a store seeded from one embedded schema fixture.\n\
         \n\
         schemas: {}\n",
        names().join(", ")
    ));
    ExitCode::from(EXIT_USAGE)
}

/// Prints a diagnostic and exits.
fn fail(message: &str) -> ExitCode {
    report(&format!("duet-host-stdio: {message}\n"));
    ExitCode::from(EXIT_FAILED)
}

/// Writes to stderr, ignoring a failure to do so.
///
/// stderr is not part of this host's protocol — stdout carries every byte a
/// guest reads — so a closed stderr must not change what the process does.
fn report(message: &str) {
    let _ = io::stderr().write_all(message.as_bytes());
}
