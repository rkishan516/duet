//! What `duet generate` actually does.
//!
//! Thin on purpose. Reading a schema, planning it and emitting two languages
//! are [`duet_codegen`]'s, and every rejection this crate can report for a bad
//! schema is that crate's own message reaching the terminal unaltered. What is
//! added here is the file system: which paths, compared or written, and what a
//! human reads afterwards.

use std::fs;
use std::io;
use std::path::Path;

use duet_codegen::{Options, generate as emit, read_schema};

use crate::args::Generate;
use crate::diff;
use crate::error::{CliError, Difference};
use crate::write::{Target, write_all};

/// What a successful run should tell the user, one line per file.
///
/// A generator that prints nothing on success is indistinguishable from one
/// that silently did nothing, and the paths are the part worth confirming —
/// a mistyped `--dart` is otherwise a file created somewhere unexpected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The lines to print, already ordered.
    pub lines: Vec<String>,
}

/// Runs one `generate` request.
///
/// With `--check`, reads each target and compares; without it, writes them all.
/// Either way the emitting happens first and completely, so a schema this tool
/// cannot emit never touches a file.
///
/// # Errors
///
/// [`CliError`] for a schema that cannot be read or parsed, a schema with no
/// faithful spelling in the target languages, an output that cannot be read
/// back or written, or — under `--check` — a file that differs. Never panics.
pub fn run(request: &Generate) -> Result<Report, CliError> {
    let targets = plan(request)?;
    if request.check {
        check(&targets)
    } else {
        write_all(&targets)?;
        Ok(Report {
            lines: targets
                .iter()
                .map(|target| format!("wrote {}", target.path.display()))
                .collect(),
        })
    }
}

/// Reads the schema and emits every file the request asked for.
///
/// Both languages are emitted before either is considered, because
/// [`duet_codegen::generate`] returns the pair or an error — so "generated
/// nothing yet" and "generated everything" are the only two states a caller
/// can observe.
fn plan(request: &Generate) -> Result<Vec<Target>, CliError> {
    let document = read_file(&request.schema).map_err(|source| CliError::ReadSchema {
        path: request.schema.clone(),
        source,
    })?;
    let schema = read_schema(&document).map_err(|source| CliError::Schema {
        path: request.schema.clone(),
        source,
    })?;
    let options = Options::new(request.schema.to_string_lossy(), request.command());
    let generated = emit(&schema, &options)?;

    let mut targets = Vec::with_capacity(2);
    if let Some(path) = &request.dart {
        targets.push(Target {
            path: path.clone(),
            content: generated.dart,
        });
    }
    if let Some(path) = &request.ts {
        targets.push(Target {
            path: path.clone(),
            content: generated.ts,
        });
    }
    Ok(targets)
}

/// Compares every target against what is on disk.
fn check(targets: &[Target]) -> Result<Report, CliError> {
    let mut differences = Vec::new();
    let mut lines = Vec::new();
    for target in targets {
        match read_target(&target.path)? {
            None => differences.push(Difference {
                path: target.path.clone(),
                missing: true,
                diff: String::new(),
            }),
            Some(committed) if committed != target.content => differences.push(Difference {
                path: target.path.clone(),
                missing: false,
                diff: diff::render(&committed, &target.content),
            }),
            Some(_) => lines.push(format!("up to date: {}", target.path.display())),
        }
    }
    if differences.is_empty() {
        Ok(Report { lines })
    } else {
        Err(CliError::Stale(differences))
    }
}

/// A target's current contents, or `None` if it does not exist.
///
/// A missing file is a difference rather than a failure: `--check` on a fresh
/// checkout of a project whose clients were never committed should say what is
/// missing and how to produce it, not report an I/O error.
fn read_target(path: &Path) -> Result<Option<String>, CliError> {
    match read_file(path) {
        Ok(text) => Ok(Some(text)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CliError::ReadOutput {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// A file as text.
///
/// A file that is not UTF-8 is reported as an I/O error naming the path rather
/// than as a decoding error, because to this tool the two have the same fix:
/// the thing at that path is not what the flag said it was.
fn read_file(path: &Path) -> Result<String, io::Error> {
    fs::read_to_string(path)
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
