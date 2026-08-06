//! The `frontend_server` stdin/stdout line protocol, as pure functions.
//!
//! `frontend_server` is the Dart CFE running as a daemon. Its protocol is
//! line-based, partly undocumented, and was reverse-engineered by Spike C
//! (`spikes/spike-c-macos/FINDINGS.md`, "Reverse-engineered protocol notes")
//! against the SDK in this repository. Because it is reverse-engineered, it is
//! the part most likely to shift under a Flutter upgrade — so all of it lives
//! here, with no I/O, behind direct tests including for output shapes this
//! machine has never produced.
//!
//! # One exchange
//!
//! ```text
//! ->  recompile <boundary>
//! ->  package:duet_guest/main.dart
//! ->  <boundary>
//! <-  <stray output from a previous `accept`, sometimes>
//! <-  result <boundary-the-server-chose>
//! <-  lib/main.dart:12:8: Error: …          (zero or more diagnostics)
//! <-  <boundary-the-server-chose> [<dill path> <error count>]
//! ```
//!
//! Three things about that are not obvious and all three cost time to learn:
//!
//! 1. **The server picks its own boundary key for the reply**, echoed after
//!    `result `. It is not necessarily the one the request carried, so the
//!    parser latches whatever it is told rather than matching against what it
//!    sent.
//! 2. **Anything before the `result ` line is stray** — `accept` sometimes
//!    echoes a delayed confirmation that arrives interleaved with the *next*
//!    command's output. Discarding pre-`result` lines is what keeps that from
//!    being mistaken for a diagnostic.
//! 3. **The terminator's trailing fields are optional in practice.** The
//!    `--help` text and `flutter_tools`' own parser
//!    (`packages/flutter_tools/lib/src/compile.dart`) both expect
//!    `<boundary> <dill> <errorCount>`; Spike C observed a *bare* boundary
//!    from this SDK. [`Terminator::parse`] accepts both, uses the error count
//!    when it is there, and falls back to reading the diagnostics when it is
//!    not — so neither shape is a silent wrong answer.

use std::fmt::Write as _;

/// The trailing part of a result block's terminator line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Terminator {
    /// The dill the server wrote, if it said.
    pub(crate) dill: Option<String>,
    /// How many errors it found, if it said.
    pub(crate) errors: Option<u32>,
}

impl Terminator {
    /// Parses the text that follows the boundary key on a terminator line.
    ///
    /// `flutter_tools` splits on the **last** space, so a dill path containing
    /// spaces still parses — this does the same rather than splitting on the
    /// first, which would break on any project under a path with a space in
    /// it. That is not hypothetical on macOS.
    pub(crate) fn parse(rest: &str) -> Self {
        let rest = rest.trim();
        if rest.is_empty() {
            return Terminator::default();
        }
        match rest.rsplit_once(' ') {
            Some((dill, count)) => match count.trim().parse::<u32>() {
                Ok(errors) => Terminator {
                    dill: Some(dill.trim().to_string()),
                    errors: Some(errors),
                },
                // A trailing token that is not a count means the whole thing
                // is a path. Better to report no count — and fall back to the
                // diagnostics — than to guess.
                Err(_) => Terminator {
                    dill: Some(rest.to_string()),
                    errors: None,
                },
            },
            None => Terminator {
                dill: Some(rest.to_string()),
                errors: None,
            },
        }
    }
}

/// Feeding one line to [`ResultParser`] either advances it or completes it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Feed {
    /// Keep reading.
    NeedMore,
    /// The block ended.
    Done(Terminator),
}

/// Accumulates one `compile`/`recompile` result block.
#[derive(Debug, Default)]
pub(crate) struct ResultParser {
    /// The boundary key the server announced, once it has.
    boundary: Option<String>,
    /// Diagnostic lines seen after the `result ` line.
    diagnostics: Vec<String>,
}

impl ResultParser {
    /// Folds in one line of the server's stdout.
    pub(crate) fn feed(&mut self, line: &str) -> Feed {
        let line = line.trim_end_matches(['\n', '\r']);
        match &self.boundary {
            None => {
                if let Some(rest) = line.strip_prefix("result ") {
                    let boundary = rest.trim();
                    // A `result ` line with nothing after it would latch an
                    // empty boundary, and then *every* subsequent line would
                    // "start with" it and terminate the block immediately.
                    // Refusing it keeps the parser waiting for a real one.
                    if !boundary.is_empty() {
                        self.boundary = Some(boundary.to_string());
                    }
                }
                // Anything else before `result ` is stray; discard it.
                Feed::NeedMore
            }
            Some(boundary) => match line.strip_prefix(boundary.as_str()) {
                Some(rest) => Feed::Done(Terminator::parse(rest)),
                None => {
                    self.diagnostics.push(line.to_string());
                    Feed::NeedMore
                }
            },
        }
    }

    /// The diagnostics collected so far, consumed.
    pub(crate) fn take_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Whether a `result` line has been seen.
    ///
    /// Lets a caller distinguish "the compiler has said nothing" — a genuine
    /// hang — from "the compiler is talking but never opened a result block",
    /// which means the line protocol changed. Those have completely different
    /// fixes, and reporting the second as a timeout would send a developer
    /// looking for a wedged process that is in fact working fine.
    pub(crate) fn started(&self) -> bool {
        self.boundary.is_some()
    }
}

/// Whether a diagnostic line is a compile error.
///
/// Only used when the terminator did not carry an error count. The CFE writes
/// errors as `<path>:<line>:<col>: Error: <message>`, and severity is always
/// one of `Error`, `Warning`, `Context` or `Info` in that position.
///
/// Deliberately narrower than Spike C's version, which also matched any line
/// containing "exception" case-insensitively. That would classify a perfectly
/// good build of a file mentioning `FormatException` in a warning as a
/// failure — and a driver that refuses to reload working code is worse than
/// one that occasionally tries to reload broken code, which the VM then simply
/// declines.
pub(crate) fn is_error(line: &str) -> bool {
    line.contains(": Error: ") || line.starts_with("Error: ")
}

/// The `compile <entrypoint>` command.
pub(crate) fn compile_command(entrypoint: &str) -> String {
    format!("compile {entrypoint}\n")
}

/// The `recompile` command: a boundary, the invalidated URIs one per line,
/// then the boundary again.
///
/// Every URI the caller passes is listed. Spike C only ever invalidated the
/// entrypoint because it only ever edited `main.dart`; a real watcher sees
/// edits anywhere in the project, and listing only the entrypoint would make
/// the compiler miss them.
pub(crate) fn recompile_command(boundary: &str, invalidated: &[String]) -> String {
    let mut out = String::with_capacity(64 + invalidated.len() * 48);
    // Writing into a String cannot fail; the results are discarded rather than
    // unwrapped so this stays panic-free by construction.
    let _ = writeln!(out, "recompile {boundary}");
    for uri in invalidated {
        let _ = writeln!(out, "{uri}");
    }
    let _ = writeln!(out, "{boundary}");
    out
}

#[cfg(test)]
#[path = "compiler_protocol_tests.rs"]
mod tests;
