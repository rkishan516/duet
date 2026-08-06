//! Writing a set of files so that a failure leaves none of them corrupt.
//!
//! # The hazard
//!
//! `duet generate` writes two files. The naive version — open, truncate, write,
//! repeat — has two ways to leave a developer worse off than before they ran it:
//!
//! - A write that fails **part-way through one file** leaves that file
//!   truncated. Half a generated Dart file still parses as far as the cut, so
//!   the failure surfaces later as a mystifying analyzer error rather than as
//!   the disk error it was.
//! - A write that fails **between the two files** leaves a new Dart client
//!   beside an old TypeScript one. Both compile. They disagree about the store.
//!
//! # The guarantee, in three parts
//!
//! 1. **Nothing is written until everything is generated.**
//!    [`duet_codegen::generate`] returns both languages or an error, so an
//!    unemittable schema never reaches this module at all. That is a property
//!    of the library's signature, not of care taken here.
//!
//! 2. **No target is ever opened for truncation.** Each file is written to a
//!    fresh temporary in the *same directory* — same directory so the rename is
//!    within one filesystem, where POSIX requires it to be atomic — and then
//!    renamed over the target. A reader of the target sees the old bytes or the
//!    new bytes, never a prefix.
//!
//! 3. **Every temporary is staged before any rename happens.** A read-only
//!    output directory, a full disk or a bad path is discovered while both
//!    targets still hold their previous contents, and the staged temporaries are
//!    removed on the way out.
//!
//! What remains, stated plainly: if the *first* `rename` succeeds and the second
//! fails, the pair is half-updated. Closing that would need a filesystem
//! transaction, which POSIX does not offer.
//!
//! It is a much smaller window than the one part 3 closes. Every failure with a
//! plausible cause happens during staging: a read-only directory, a full disk,
//! a path that cannot be created — and a target that is itself a directory,
//! which staging pre-flights precisely because it is the one mistake that
//! would otherwise stage cleanly and fail at the rename. `--dart lib/` is easy
//! to type, and the first version of this module let it replace the TypeScript
//! client before objecting. A rename that fails for any other reason means the
//! filesystem changed underneath the process mid-run.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One file's worth of output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Where it goes.
    pub path: PathBuf,
    /// What goes in it.
    pub content: String,
}

/// Why a set of files could not be written.
///
/// Carries the path as well as the cause: `Permission denied (os error 13)` on
/// its own is the least useful true statement a tool can make.
#[derive(Debug)]
pub struct WriteError {
    /// The file being written, or the directory being created.
    pub path: PathBuf,
    /// What was being attempted.
    pub doing: &'static str,
    /// The operating system's objection.
    pub source: io::Error,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot {} {}: {}",
            self.doing,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Writes every target, or leaves every target as it was.
///
/// See the module documentation for exactly what "or" means here.
///
/// # Errors
///
/// [`WriteError`] naming the path and the operating system's objection, for a
/// directory that cannot be created, a temporary that cannot be written, or a
/// rename that fails. Never panics.
pub fn write_all(targets: &[Target]) -> Result<(), WriteError> {
    let mut staged: Vec<(PathBuf, &Path)> = Vec::with_capacity(targets.len());
    for target in targets {
        match stage(target) {
            Ok(temporary) => staged.push((temporary, &target.path)),
            Err(e) => {
                discard(&staged);
                return Err(e);
            }
        }
    }
    for (temporary, path) in &staged {
        // Uncovered by the suite, and deliberately kept. Every cause of a
        // rename failure that a test can arrange — a read-only directory, an
        // uncreatable path, a target that is a directory — is now refused
        // during staging, which is the whole point of the two phases. What is
        // left needs the filesystem to change under the process between the two
        // loops, and a branch that only a race can reach is exactly the one
        // worth keeping rather than replacing with `expect`.
        if let Err(source) = fs::rename(temporary, path) {
            // The already-renamed ones are complete files, so only the
            // still-staged temporaries are litter worth clearing.
            discard(&staged);
            return Err(WriteError {
                path: path.to_path_buf(),
                doing: "move the generated file into place at",
                source,
            });
        }
    }
    Ok(())
}

/// Writes one target's temporary, creating its directory.
///
/// The directory pre-flight is here rather than left to `rename` for a measured
/// reason: staging a temporary beside a target that is itself a directory
/// *succeeds* — the temporary has a different name — and the failure then lands
/// on the rename, after a sibling target may already have been replaced. That
/// is the one plausible cause of a late failure, and `--dart lib/` is an easy
/// thing to type, so it is caught while every target is still untouched.
fn stage(target: &Target) -> Result<PathBuf, WriteError> {
    if target.path.is_dir() {
        return Err(WriteError {
            path: target.path.clone(),
            doing: "write the generated file to",
            source: io::Error::new(
                io::ErrorKind::IsADirectory,
                "it is a directory; name the file to write, not the directory \
                 to write it in",
            ),
        });
    }
    let directory = target.path.parent().unwrap_or_else(|| Path::new("."));
    // An empty parent is what `Path::parent` gives for a bare file name, and
    // `create_dir_all("")` fails. `.` always exists, so this is a no-op there.
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    fs::create_dir_all(directory).map_err(|source| WriteError {
        path: directory.to_path_buf(),
        doing: "create the output directory",
        source,
    })?;
    let temporary = temporary_path(&target.path);
    fs::write(&temporary, &target.content).map_err(|source| WriteError {
        path: temporary.clone(),
        doing: "write",
        source,
    })?;
    Ok(temporary)
}

/// Where a target's temporary lives.
///
/// Beside the target, so the rename stays on one filesystem, and stamped with
/// the process id so two `duet generate` runs sharing an output directory
/// cannot stage over each other.
fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    directory.join(format!(".{name}.duet-{}.tmp", std::process::id()))
}

/// Removes temporaries that will not be used.
///
/// A failure to remove one is ignored on purpose: the caller is already on its
/// way out with a more interesting error, and replacing that error with
/// "and also, the temporary file could not be deleted" helps nobody.
fn discard(staged: &[(PathBuf, &Path)]) {
    for (temporary, _) in staged {
        let _ = fs::remove_file(temporary);
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
