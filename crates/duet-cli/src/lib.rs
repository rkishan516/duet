//! `duet` — typed Dart and TypeScript clients from a Duet schema.
//!
//! This is the first piece of Duet a third party runs rather than links. It is
//! deliberately **thin**: every transformation it performs is a call into
//! [`duet_codegen`], which the goldens, the round-trip cross-check, the
//! real-host path check and both guest conformance suites already gate. What
//! this crate adds is a command line, a file system, and exit codes.
//!
//! # The pipeline it sits in
//!
//! ```text
//! #[derive(SharedState)]  ->  schema.json  ->  duet generate  ->  Dart
//! on a Rust struct            the contract     this crate         TypeScript
//! ```
//!
//! The schema document in the middle is the contract, and it has two
//! independent producers: a human writing the JSON, and the derive macro, which
//! `crates/duet-derive/tests/schema_proof.rs` holds to reproducing the
//! hand-written specification byte for byte. Neither this tool nor the derive
//! can bless the format on its own.
//!
//! # Two commands, and no more
//!
//! ```text
//! duet generate --schema <path> --dart <path> --ts <path>
//! duet generate --schema <path> --dart <path> --ts <path> --check
//! ```
//!
//! `--check` writes nothing and exits 3 if any output differs from what would
//! be generated, printing the differing line and the exact command that fixes
//! it. That is what makes a stale committed client a build failure for a user
//! outside this workspace, where `cargo test -p duet-codegen` is not available.
//!
//! # Totality
//!
//! A schema file, a path and argv are all inputs this tool does not control.
//! Nothing outside `#[cfg(test)]` calls `unwrap`, `expect` or `panic!`, no
//! message echoes an untrusted string without a bound, and every failure is a
//! [`CliError`] carrying an exit code. A malformed schema, an unreadable path
//! and a read-only output directory are three different messages, each naming
//! the fix.
//!
//! # Testing shape
//!
//! [`execute`] takes arguments and returns the bytes and the exit code rather
//! than touching the process, so the whole tool is reachable from a unit test.
//! `tests/cli.rs` nevertheless drives the **real binary**, because argv, exit
//! codes and the two output streams are the parts a library call cannot check.
//!
//! ```
//! # let dir = std::env::temp_dir().join(format!("duet-cli-doc-{}", std::process::id()));
//! # std::fs::create_dir_all(&dir).unwrap();
//! # let schema = dir.join("app.json");
//! # std::fs::write(&schema, r#"{"root":{"kind":"named","name":"App"},"types":[{"fields":[{"key":"counter","type":{"kind":"int"}}],"name":"App"}],"version":1}"#).unwrap();
//! # let dart = dir.join("app.duet.dart");
//! let outcome = duet_cli::execute(&[
//!     "generate",
//!     "--schema",
//!     schema.to_str().unwrap(),
//!     "--dart",
//!     dart.to_str().unwrap(),
//! ]);
//!
//! assert_eq!(outcome.code, 0);
//! assert!(outcome.out.starts_with("wrote "));
//! assert!(std::fs::read_to_string(&dart).unwrap().contains("get counter"));
//! # std::fs::remove_dir_all(&dir).unwrap();
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod args;
pub mod dev;
pub mod diff;
pub mod error;
pub mod help;
pub mod report;
pub mod run;
pub mod write;

pub use args::{Dev, Generate, Invocation, UsageError};
pub use error::{CliError, Difference, EXIT_FAILED, EXIT_STALE, EXIT_USAGE};
pub use run::{Report, run};
pub use write::{Target, WriteError};

/// Everything one invocation produces, without a process being involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// What the process should exit with.
    pub code: u8,
    /// What belongs on stdout.
    pub out: String,
    /// What belongs on stderr.
    pub err: String,
}

/// Runs `duet` with `arguments`, which exclude the program name.
///
/// Total: every input produces an [`Outcome`], and nothing here panics.
/// Diagnostics go to [`Outcome::err`] and everything else to [`Outcome::out`],
/// so a caller redirecting one stream never loses the other.
///
/// **One exception, and it is deliberate: `dev`.** A `duet dev` session runs
/// until the host exits or the developer interrupts it, so buffering its
/// output into an [`Outcome`] would show them nothing at all until it ended.
/// That arm streams straight to this process's own stdout and stderr and
/// returns an [`Outcome`] carrying only the exit code. Callers that need to
/// capture a session's output should use [`dev::run`], which takes the two
/// writers explicitly — that is how this crate's own tests drive it.
pub fn execute<S: AsRef<str>>(arguments: &[S]) -> Outcome {
    match args::parse(arguments) {
        Ok(Invocation::Help) => succeed(help::HELP),
        Ok(Invocation::GenerateHelp) => succeed(help::GENERATE_HELP),
        Ok(Invocation::DevHelp) => succeed(help::DEV_HELP),
        Ok(Invocation::Version) => succeed(&help::version()),
        Ok(Invocation::Generate(request)) => generate(&request),
        Ok(Invocation::Dev(request)) => Outcome {
            code: dev::run(
                &request,
                &mut std::io::stdout().lock(),
                &mut std::io::stderr().lock(),
            ),
            out: String::new(),
            err: String::new(),
        },
        Err(e) => fail(&CliError::Usage(e), None),
    }
}

/// A `generate` request, run.
fn generate(request: &Generate) -> Outcome {
    match run::run(request) {
        Ok(report) => {
            let mut out = report.lines.join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            Outcome {
                code: 0,
                out,
                err: String::new(),
            }
        }
        // The command a failure names is the canonical one, never the argv that
        // produced it — see `Generate::command`.
        Err(e) => fail(&e, Some(&request.command())),
    }
}

/// Text on stdout, exit 0.
fn succeed(text: &str) -> Outcome {
    Outcome {
        code: 0,
        out: text.to_string(),
        err: String::new(),
    }
}

/// A diagnostic on stderr, and the error's own exit code.
fn fail(error: &CliError, command: Option<&str>) -> Outcome {
    Outcome {
        code: error.exit_code(),
        out: String::new(),
        err: report::failure(error, command),
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
