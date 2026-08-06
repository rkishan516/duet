//! What can go wrong, and — always — *where*.
//!
//! # Every error names a stage
//!
//! A hot-reload cycle is a chain of five things that can each hang forever: a
//! child compiler's pipe, a TCP connect, a JSON-RPC round trip, a second
//! JSON-RPC round trip, and a filesystem poll. "Reload timed out" tells a
//! developer nothing they can act on. "Timed out after 10s at stage
//! `ReloadSources`" tells them the compiler is fine, the socket is up, and the
//! Dart VM is wedged — which is a completely different morning.
//!
//! So [`Stage`] is not decoration on a couple of variants: every single
//! [`DevError`] carries one, there is no constructor that omits it, and
//! [`DevError::stage`] is infallible. That is the whole design of this module.
//!
//! # No `unwrap`, no `expect`, no `panic!`
//!
//! A dev tool that panics on a syntax error is worse than useless: the
//! developer's next action is to fix the syntax error, and a panicked driver
//! means they also have to restart the whole session first. Every fallible
//! thing in this crate returns a `Result` whose error lands here, and a Dart
//! compile error is not even an error — see [`crate::CompileOutcome`], which
//! models it as a normal outcome of a successful call.

use std::fmt;
use std::time::Duration;

/// Where in a reload cycle something happened.
///
/// Ordered roughly as a cycle runs, which is also the order a reader will scan
/// this list in when matching it against a failure they just saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Stage {
    /// Resolving the Flutter SDK's `dartaotruntime`, `frontend_server`
    /// snapshot and patched SDK root from a Flutter root directory.
    LocateSdk,
    /// Starting the `frontend_server` child process.
    SpawnCompiler,
    /// The one-off full `compile` that primes the incremental compiler.
    BaselineCompile,
    /// Discovering the Dart VM service URI.
    LocateVmService,
    /// Opening the WebSocket to the Dart VM service.
    Connect,
    /// `getVM`, and picking the isolate to reload.
    FindIsolate,
    /// An incremental `recompile`.
    Recompile,
    /// The `accept` that commits a generation as the next baseline.
    Accept,
    /// The `reloadSources` RPC.
    ReloadSources,
    /// The `ext.flutter.reassemble` RPC that rebuilds the widget tree.
    Reassemble,
    /// Waiting for the reloaded code's effect to become observable.
    Observe,
    /// Scanning the watched tree for changes.
    Watch,
}

impl Stage {
    /// A short, stable, lower-case name — for logs and for the `duet dev`
    /// status line.
    ///
    /// Deliberately not derived from [`fmt::Debug`]: these strings are what a
    /// developer greps for and what a future `--json` mode would emit, so
    /// renaming a variant must not silently rename them.
    pub fn name(self) -> &'static str {
        match self {
            Stage::LocateSdk => "locate-sdk",
            Stage::SpawnCompiler => "spawn-compiler",
            Stage::BaselineCompile => "baseline-compile",
            Stage::LocateVmService => "locate-vm-service",
            Stage::Connect => "connect",
            Stage::FindIsolate => "find-isolate",
            Stage::Recompile => "recompile",
            Stage::Accept => "accept",
            Stage::ReloadSources => "reload-sources",
            Stage::Reassemble => "reassemble",
            Stage::Observe => "observe",
            Stage::Watch => "watch",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The longest error detail this crate will carry, in bytes.
///
/// Compiler diagnostics and VM-service payloads are effectively unbounded —
/// `getVM` against a large app is tens of kilobytes, and a pathological Dart
/// file can produce megabytes of errors. An error type that embeds all of it
/// makes every `?` a large memcpy and every log line unreadable. The full text
/// is still shown where it belongs (compiler diagnostics reach the developer
/// through [`crate::CompileOutcome::diagnostics`], untruncated); this bound is
/// only on text folded into an *error message*.
const MAX_DETAIL: usize = 2048;

/// Truncates `text` to [`MAX_DETAIL`] bytes on a character boundary, marking
/// it when it does.
///
/// Splitting on a `char` boundary rather than a byte index is not fussiness:
/// Dart diagnostics routinely contain non-ASCII (a quoted identifier, a
/// `→`-arrow in a CFE message), and slicing one mid-codepoint would panic —
/// in the middle of building an error value, which is the worst possible place
/// for this crate to panic.
pub(crate) fn truncate(text: &str) -> String {
    if text.len() <= MAX_DETAIL {
        return text.to_string();
    }
    let mut end = MAX_DETAIL;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &text[..end], text.len())
}

/// Everything this crate can fail with.
///
/// `#[non_exhaustive]` because `duet dev` will grow stages (a web/Vite half,
/// per the spec's §8.2 diagram) and a caller matching exhaustively today
/// should not break when it does.
#[derive(Debug)]
#[non_exhaustive]
pub enum DevError {
    /// A stage did not finish within its deadline.
    ///
    /// The single most important variant in this enum. Nothing in a reload
    /// cycle is allowed to block without one of these waiting behind it.
    Timeout {
        /// Where it hung.
        stage: Stage,
        /// The deadline that elapsed.
        after: Duration,
    },

    /// The `frontend_server` child process is gone.
    ///
    /// Distinct from [`DevError::Io`] on purpose: a closed pipe reads as an
    /// ordinary EOF, and reporting that as "unexpected end of file" would send
    /// a developer looking at their source tree instead of at the compiler
    /// that just died. Carries the exit status when the child had already been
    /// reaped, and the tail of its stderr, which is where the actual reason
    /// lives (a bad `--sdk-root`, an SDK/snapshot version mismatch).
    CompilerExited {
        /// Where the death was noticed.
        stage: Stage,
        /// The child's exit code, if it could be collected.
        status: Option<i32>,
        /// The tail of the child's stderr, truncated.
        stderr: String,
    },

    /// The `frontend_server` said something this crate cannot parse.
    ///
    /// Its stdout protocol is line-based and reverse-engineered (see
    /// `frontend_server.rs`); a future SDK could change it. That must surface
    /// as a clear "the compiler said something unexpected", not as a hang or a
    /// wrong answer.
    CompilerProtocol {
        /// Where the garbage arrived.
        stage: Stage,
        /// What was wrong, and the offending text, truncated.
        detail: String,
    },

    /// The Dart VM service refused, or answered something unusable.
    VmService {
        /// Where it happened.
        stage: Stage,
        /// The JSON-RPC error or the shape problem, truncated.
        detail: String,
    },

    /// A required file or directory is missing or unusable.
    ///
    /// Separated from [`DevError::Io`] because the fix is different: this one
    /// is nearly always a wrong path in the caller's configuration, and the
    /// message names the path so it can be checked directly.
    NotFound {
        /// Where it was needed.
        stage: Stage,
        /// What was being looked for.
        what: &'static str,
        /// The path that was not there.
        path: String,
    },

    /// Any other I/O failure, with the stage that was running.
    Io {
        /// Where it happened.
        stage: Stage,
        /// What was being attempted.
        doing: &'static str,
        /// The underlying error.
        source: std::io::Error,
    },
}

impl DevError {
    /// The stage this error happened at. Infallible by construction: there is
    /// no variant without one, which is the point of this type.
    pub fn stage(&self) -> Stage {
        match self {
            DevError::Timeout { stage, .. }
            | DevError::CompilerExited { stage, .. }
            | DevError::CompilerProtocol { stage, .. }
            | DevError::VmService { stage, .. }
            | DevError::NotFound { stage, .. }
            | DevError::Io { stage, .. } => *stage,
        }
    }

    /// Builds a [`DevError::CompilerProtocol`], truncating `detail`.
    pub(crate) fn protocol(stage: Stage, detail: impl Into<String>) -> Self {
        DevError::CompilerProtocol {
            stage,
            detail: truncate(&detail.into()),
        }
    }

    /// Builds a [`DevError::VmService`], truncating `detail`.
    pub(crate) fn vm(stage: Stage, detail: impl Into<String>) -> Self {
        DevError::VmService {
            stage,
            detail: truncate(&detail.into()),
        }
    }
}

impl fmt::Display for DevError {
    /// Every message leads with the stage, because that is the first thing a
    /// reader needs and the last thing they should have to dig for.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DevError::Timeout { stage, after } => {
                write!(f, "[{stage}] timed out after {after:?}")
            }
            DevError::CompilerExited {
                stage,
                status,
                stderr,
            } => {
                write!(f, "[{stage}] the frontend_server process exited")?;
                match status {
                    Some(code) => write!(f, " with status {code}")?,
                    None => write!(f, " (status not collected)")?,
                }
                if stderr.is_empty() {
                    write!(f, "; it wrote nothing to stderr")
                } else {
                    write!(f, "; its stderr ended with: {stderr}")
                }
            }
            DevError::CompilerProtocol { stage, detail } => {
                write!(
                    f,
                    "[{stage}] the frontend_server said something unexpected: {detail}"
                )
            }
            DevError::VmService { stage, detail } => {
                write!(f, "[{stage}] the Dart VM service: {detail}")
            }
            DevError::NotFound { stage, what, path } => {
                write!(f, "[{stage}] no {what} at {path}")
            }
            DevError::Io {
                stage,
                doing,
                source,
            } => {
                write!(f, "[{stage}] {doing}: {source}")
            }
        }
    }
}

impl std::error::Error for DevError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DevError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
