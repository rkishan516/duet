//! Why a run stopped, and what the process exits with.
//!
//! # Four exit codes, because a script has four different reactions
//!
//! A caller that can only see "zero or not" has to parse stderr to tell a typo
//! in its own command line from a stale committed file, and parsing another
//! tool's prose is how scripts break silently. So:
//!
//! | Code | Meaning | What a caller does |
//! |---|---|---|
//! | 0 | written, or up to date | carry on |
//! | 1 | the run failed | surface the message; it is an environment problem |
//! | 2 | the command line was wrong | fix the invocation; retrying will not help |
//! | 3 | `--check` found a difference | run the printed command and commit the diff |
//!
//! 3 being distinct from 1 is the load-bearing one, and it is what the CI step
//! relies on: a schema that no longer parses and a client that is merely stale
//! are different problems with different fixes, and a gate that reported them
//! identically would train everyone to run the regeneration command at both.

use std::fmt;
use std::io;
use std::path::PathBuf;

use duet_codegen::{EmitError, ReadError};

use crate::args::UsageError;
use crate::write::WriteError;

/// The command line was wrong.
pub const EXIT_USAGE: u8 = 2;

/// The run failed.
pub const EXIT_FAILED: u8 = 1;

/// `--check` found a file that differs.
pub const EXIT_STALE: u8 = 3;

/// One committed file that is not what would be generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// The file, as it was named on the command line.
    pub path: PathBuf,
    /// Whether it is missing entirely, rather than merely different.
    pub missing: bool,
    /// The rendered diff, empty when the file is missing.
    pub diff: String,
}

/// Why `duet` stopped.
#[derive(Debug)]
#[non_exhaustive]
pub enum CliError {
    /// argv did not name a run this tool can do.
    Usage(UsageError),
    /// The schema file could not be read off the disk.
    ReadSchema {
        /// The path that was tried.
        path: PathBuf,
        /// The operating system's objection.
        source: io::Error,
    },
    /// The schema file was read but is not a schema.
    Schema {
        /// The path it came from.
        path: PathBuf,
        /// What is wrong with it.
        source: ReadError,
    },
    /// The schema is valid but has no faithful spelling in Dart and TypeScript.
    Emit(EmitError),
    /// An output file could not be compared against what is on disk.
    ReadOutput {
        /// The path that was tried.
        path: PathBuf,
        /// The operating system's objection.
        source: io::Error,
    },
    /// An output file could not be written.
    Write(WriteError),
    /// `--check` found one or more files that differ.
    Stale(Vec<Difference>),
}

impl CliError {
    /// What the process should exit with.
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Usage(_) => EXIT_USAGE,
            CliError::Stale(_) => EXIT_STALE,
            _ => EXIT_FAILED,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(e) => write!(f, "{e}"),
            CliError::ReadSchema { path, source } => {
                write!(f, "cannot read the schema at {}: {source}", path.display())
            }
            CliError::Schema { path, source } => {
                write!(f, "{} is not a valid schema: {source}", path.display())
            }
            CliError::Emit(e) => write!(f, "{e}"),
            CliError::ReadOutput { path, source } => {
                write!(f, "cannot read {} to compare it: {source}", path.display())
            }
            CliError::Write(e) => write!(f, "{e}"),
            // The whole point of `--check` is the per-file report, which needs
            // more than one line and is written by `report::stale`. This is the
            // summary that precedes it.
            CliError::Stale(differences) => {
                write!(f, "{} generated file(s) are out of date", differences.len())
            }
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::Usage(e) => Some(e),
            CliError::ReadSchema { source, .. } | CliError::ReadOutput { source, .. } => {
                Some(source)
            }
            CliError::Schema { source, .. } => Some(source),
            CliError::Emit(e) => Some(e),
            CliError::Write(e) => Some(e),
            CliError::Stale(_) => None,
        }
    }
}

impl From<UsageError> for CliError {
    fn from(error: UsageError) -> CliError {
        CliError::Usage(error)
    }
}

impl From<EmitError> for CliError {
    fn from(error: EmitError) -> CliError {
        CliError::Emit(error)
    }
}

impl From<WriteError> for CliError {
    fn from(error: WriteError) -> CliError {
        CliError::Write(error)
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
