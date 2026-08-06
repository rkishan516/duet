//! argv, parsed.
//!
//! # Why there is no argument-parsing crate here
//!
//! `clap` is the obvious answer and it was weighed rather than dismissed. It
//! was not taken, for three reasons that are specific to this tool rather than
//! general:
//!
//! 1. **The surface is two subcommands and seven flags**, and it is meant to
//!    stay small. `clap` with `default-features = false` still brings
//!    `clap_builder`, `clap_lex` and `anstyle` — tens of thousands of lines to
//!    decide whether a string equals `"--schema"`. This workspace argues for
//!    every dependency it takes: `duet-core` has none by design and CI asserts
//!    it, `packages/duet-js` has no runtime dependency and CI asserts that too,
//!    and `crates/duet-host-stdio` already hand-rolls its own argv. A tool whose
//!    whole job is to be the framework's front door is a poor place to start
//!    being casual about the dependency tree.
//!
//! 2. **The `--help` text is the one place a newcomer meets Duet**, so it is
//!    written rather than generated. `clap`'s derived help would need
//!    `long_about` overrides on every item to say anything like what
//!    [`crate::help`] says, at which point the generator is only laying out
//!    text somebody wrote anyway.
//!
//! 3. **`clap`'s value taken for free is exactly what is cheapest to test.**
//!    Hand-rolled parsing earns its keep only if every arm is pinned, and every
//!    arm is: see the tests at the foot of this file.
//!
//! What is given up is real and worth naming: `clap` would supply shell
//! completions, `--flag=value` handling, near-miss suggestions and coloured
//! errors without any of them being this crate's problem. `--flag=value` is
//! implemented here because omitting it makes a path beginning with `-`
//! unspellable; the rest are not, and if this tool ever grows a third
//! subcommand that trade should be revisited.
//!
//! # What is rejected, and why each one is rejected rather than tolerated
//!
//! - **A repeated flag.** Taking the last silently discards the first, and
//!   `duet generate --dart a.dart --dart b.dart` writing only `b.dart` is a
//!   footgun in a script that appends arguments.
//! - **A flag with no value.** `--dart` at the end of argv is a truncated
//!   command line, not a request to generate to an empty path.
//! - **A value that looks like a flag.** `--dart --ts out.ts` would otherwise
//!   generate a Dart file literally named `--ts`. A real path starting with `-`
//!   is still reachable as `--dart=-weird.dart`.
//! - **Neither `--dart` nor `--ts`.** The alternative is a successful run that
//!   writes nothing, which is the worst outcome a generator has: exit 0 and no
//!   output looks exactly like a cache hit.

use std::fmt;
use std::path::PathBuf;

/// What one invocation of `duet` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Print [`help`](crate::help::HELP) and exit successfully.
    Help,
    /// Print [`the version`](crate::help::version) and exit successfully.
    Version,
    /// Print the `generate` help and exit successfully.
    GenerateHelp,
    /// Print the `dev` help and exit successfully.
    DevHelp,
    /// Generate, or check, a set of clients.
    Generate(Generate),
    /// Run a host and hot-reload its Dart guest on save.
    Dev(Dev),
}

/// A `duet dev` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dev {
    /// The Flutter project directory: the one holding `pubspec.yaml` and
    /// `.dart_tool/`.
    pub project: PathBuf,
    /// The Flutter SDK checkout, when it was given on the command line.
    ///
    /// `None` means "take it from `FLUTTER_ROOT`", resolved at run time rather
    /// than here — a parser that read the environment would make its own tests
    /// depend on the machine running them.
    pub flutter_root: Option<PathBuf>,
    /// The Dart entrypoint, when it was overridden.
    ///
    /// `None` derives it from the project's own package name, which is what a
    /// standard `lib/main.dart` layout wants.
    pub entrypoint: Option<String>,
    /// The command that runs the host, taken from everything after `--`.
    ///
    /// A `Vec`, not a string to be split: a host command routinely contains a
    /// path with a space in it, and splitting on whitespace would break that
    /// in a way no quoting the developer tried could fix.
    pub host: Vec<String>,
}

/// A `duet generate` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generate {
    /// The schema document to read.
    pub schema: PathBuf,
    /// Where to put the Dart client, if one was asked for.
    pub dart: Option<PathBuf>,
    /// Where to put the TypeScript client, if one was asked for.
    pub ts: Option<PathBuf>,
    /// Compare against what is on disk instead of writing.
    pub check: bool,
}

impl Generate {
    /// The exact command that produces these files, in canonical flag order.
    ///
    /// Two things read this string, and they have to agree: it is written into
    /// every generated file's header as the way to regenerate it, and it is
    /// printed by a failing `--check` as the way to fix the difference.
    ///
    /// **Canonical order, not the order argv used**, and `--check` is always
    /// dropped. Both matter. If the header echoed argv, then generating with
    /// the flags swapped would produce a different file from the same schema,
    /// and `--check` would report a difference that no change caused. And a
    /// header naming a `--check` invocation would tell the reader to run the
    /// command that does not write anything.
    pub fn command(&self) -> String {
        let mut command = format!("duet generate --schema {}", quote(&self.schema));
        if let Some(dart) = &self.dart {
            command.push_str(&format!(" --dart {}", quote(dart)));
        }
        if let Some(ts) = &self.ts {
            command.push_str(&format!(" --ts {}", quote(ts)));
        }
        command
    }
}

/// A path as a shell would need it written.
///
/// The command this appears in is meant to be pasted into a terminal, so a path
/// holding a space or a quote has to survive the trip. Anything outside a
/// conservative safe set is single-quoted, and an embedded single quote is
/// closed, escaped and reopened — the one escape POSIX `sh` gives inside single
/// quotes.
fn quote(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    let safe = |c: char| c.is_ascii_alphanumeric() || "._-/@+=,:".contains(c);
    if !text.is_empty() && text.chars().all(safe) {
        return text.into_owned();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// One reason argv is not an [`Invocation`].
///
/// Every variant's message names what to run instead, because a usage failure
/// is the one error whose reader is always a human at a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UsageError {
    /// argv was empty.
    NoCommand,
    /// The first argument names no subcommand.
    UnknownCommand {
        /// What was written.
        found: String,
    },
    /// A flag this subcommand does not define.
    UnknownFlag {
        /// The subcommand that does not define it.
        command: &'static str,
        /// What was written.
        found: String,
    },
    /// A bare word where a flag was expected.
    UnexpectedArgument {
        /// The subcommand that takes no positional arguments.
        command: &'static str,
        /// What was written.
        found: String,
    },
    /// A flag appeared twice.
    RepeatedFlag {
        /// The flag.
        flag: &'static str,
    },
    /// A flag that takes a value ended argv.
    MissingValue {
        /// The flag.
        flag: &'static str,
    },
    /// A flag that takes no value was given one.
    UnexpectedValue {
        /// The flag.
        flag: &'static str,
    },
    /// A flag's value is itself flag-shaped.
    FlagShapedValue {
        /// The flag.
        flag: &'static str,
        /// The value that looks like a flag.
        found: String,
    },
    /// `--schema` was not given.
    NoSchema,
    /// Neither `--dart` nor `--ts` was given.
    NoOutput,
    /// `--flutter` was not given.
    NoProject,
    /// `dev` was given no host command after `--`.
    NoHostCommand,
}

/// The most characters of an argument any message here will echo.
///
/// argv is an input like any other and a caller can pass a megabyte of it; a
/// message that quoted one whole would turn a usage error into a way to flood
/// whatever collects stderr.
const ECHO_LIMIT: usize = 64;

/// `text`, cut to [`ECHO_LIMIT`] characters on a character boundary.
fn truncate(text: &str) -> String {
    match text.char_indices().nth(ECHO_LIMIT) {
        None => text.to_string(),
        Some((at, _)) => format!("{}…", &text[..at]),
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::NoCommand => {
                write!(
                    f,
                    "no command given; run `duet --help` to see what there is"
                )
            }
            UsageError::UnknownCommand { found } => write!(
                f,
                "there is no `{}` command; the commands are `generate` and `dev`",
                truncate(found)
            ),
            UsageError::UnknownFlag { command, found } => write!(
                f,
                "`{command}` has no {} flag; it takes {}",
                truncate(found),
                flags_of(command)
            ),
            UsageError::UnexpectedArgument { command, found } => write!(
                f,
                "`{command}` takes no positional arguments, but got \"{}\"; {}",
                truncate(found),
                match *command {
                    "dev" =>
                        "every value is named by a flag, and the host command \
                              goes after `--`",
                    _ => "every path is named by a flag",
                }
            ),
            UsageError::RepeatedFlag { flag } => write!(
                f,
                "{flag} was given more than once; it takes one value, and \
                 keeping only the last would silently discard the first"
            ),
            UsageError::MissingValue { flag } => {
                write!(f, "{flag} needs a path after it")
            }
            UsageError::UnexpectedValue { flag } => write!(
                f,
                "{flag} takes no value; write it on its own to turn it on, or \
                 leave it out to turn it off"
            ),
            UsageError::FlagShapedValue { flag, found } => write!(
                f,
                "{flag} was followed by \"{}\", which looks like a flag rather \
                 than a path; write {flag}={} if that really is the path",
                truncate(found),
                truncate(found)
            ),
            UsageError::NoSchema => write!(
                f,
                "--schema is required; it names the schema document to generate from"
            ),
            UsageError::NoOutput => write!(
                f,
                "nothing to generate: pass --dart, --ts, or both, naming where \
                 each client should go"
            ),
            UsageError::NoProject => write!(
                f,
                "--flutter is required; it names the Flutter project directory \
                 holding pubspec.yaml and .dart_tool/"
            ),
            UsageError::NoHostCommand => write!(
                f,
                "no host command: write it after `--`, as in \
                 `duet dev --flutter ./flutter -- cargo run`. `dev` starts the \
                 host itself, because that is how it reads the Dart VM service \
                 URI the engine prints"
            ),
        }
    }
}

/// The flags a subcommand takes, as one readable list.
///
/// Built from the same constants the parser matches against, so a flag added
/// to one and not the other cannot produce a message that lies.
fn flags_of(command: &str) -> String {
    let (flags, extra) = match command {
        "dev" => (&DEV_FLAGS[..], ""),
        _ => (&PATH_FLAGS[..], " and --check"),
    };
    format!("{}{extra}", flags.join(", "))
}

impl std::error::Error for UsageError {}

/// Parses argv **without** the program name.
///
/// # Errors
///
/// [`UsageError`] for anything this tool does not define. Never panics,
/// whatever the arguments.
pub fn parse<S: AsRef<str>>(arguments: &[S]) -> Result<Invocation, UsageError> {
    let arguments: Vec<&str> = arguments.iter().map(AsRef::as_ref).collect();
    let Some((command, rest)) = arguments.split_first() else {
        return Err(UsageError::NoCommand);
    };
    match *command {
        "-h" | "--help" | "help" => Ok(Invocation::Help),
        "-V" | "--version" | "version" => Ok(Invocation::Version),
        "generate" => parse_generate(rest),
        "dev" => parse_dev(rest),
        other => Err(UsageError::UnknownCommand {
            found: other.to_string(),
        }),
    }
}

/// Every flag of `duet dev` that takes a value.
///
/// The same single-list discipline as [`PATH_FLAGS`]: the parser, the "unknown
/// flag" message and the help text all read this, so they cannot drift.
pub const DEV_FLAGS: [&str; 3] = ["--flutter", "--flutter-root", "--entrypoint"];

/// The flags of `duet dev`, and the host command after `--`.
///
/// `--` is handled before anything else on each pass, because everything after
/// it belongs to another program: `duet dev --flutter x -- cargo run --release`
/// must pass `--release` to `cargo`, not reject it as a flag `dev` does not
/// define.
fn parse_dev(arguments: &[&str]) -> Result<Invocation, UsageError> {
    let mut fields = DevFields::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index];
        index += 1;
        if argument == "--" {
            fields.host = arguments[index..]
                .iter()
                .map(|a| (*a).to_string())
                .collect();
            break;
        }
        let Some((flag, inline)) = split_flag(argument) else {
            return Err(UsageError::UnexpectedArgument {
                command: "dev",
                found: argument.to_string(),
            });
        };
        if matches!(flag, "-h" | "--help") {
            return Ok(Invocation::DevHelp);
        }
        // Re-derived as a `&'static str`, so a diagnostic can only name a flag
        // this parser defines.
        let flag = DEV_FLAGS
            .into_iter()
            .find(|known| *known == flag)
            .ok_or_else(|| UsageError::UnknownFlag {
                command: "dev",
                found: flag.to_string(),
            })?;
        let value = match inline {
            Some(value) => value,
            None => take_value(flag, arguments, &mut index)?,
        };
        fields.set(flag, value)?;
    }
    fields.finish().map(Invocation::Dev)
}

/// The `dev` flags seen so far, each at most once.
#[derive(Default)]
struct DevFields {
    project: Option<PathBuf>,
    flutter_root: Option<PathBuf>,
    entrypoint: Option<String>,
    host: Vec<String>,
}

impl DevFields {
    /// Records one flag, refusing a repeat for the same reason `generate`
    /// does: taking the last silently discards the first.
    fn set(&mut self, flag: &'static str, value: &str) -> Result<(), UsageError> {
        let occupied = match flag {
            "--flutter" => {
                let seen = self.project.is_some();
                self.project = Some(PathBuf::from(value));
                seen
            }
            "--flutter-root" => {
                let seen = self.flutter_root.is_some();
                self.flutter_root = Some(PathBuf::from(value));
                seen
            }
            // Unreachable: the caller matches this same set.
            _ => {
                let seen = self.entrypoint.is_some();
                self.entrypoint = Some(value.to_string());
                seen
            }
        };
        if occupied {
            return Err(UsageError::RepeatedFlag { flag });
        }
        Ok(())
    }

    /// The request, if enough of it was given.
    fn finish(self) -> Result<Dev, UsageError> {
        let DevFields {
            project,
            flutter_root,
            entrypoint,
            host,
        } = self;
        let project = project.ok_or(UsageError::NoProject)?;
        if host.is_empty() {
            return Err(UsageError::NoHostCommand);
        }
        Ok(Dev {
            project,
            flutter_root,
            entrypoint,
            host,
        })
    }
}

/// Every flag of `duet generate` that takes a path.
///
/// The single list, so the parser, the "unknown flag" message and the help text
/// cannot drift apart without a test noticing.
pub const PATH_FLAGS: [&str; 3] = ["--schema", "--dart", "--ts"];

/// The flags of `duet generate`.
fn parse_generate(arguments: &[&str]) -> Result<Invocation, UsageError> {
    let mut fields = Fields::default();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index];
        index += 1;
        let Some((flag, inline)) = split_flag(argument) else {
            return Err(UsageError::UnexpectedArgument {
                command: "generate",
                found: argument.to_string(),
            });
        };
        match flag {
            "-h" | "--help" => return Ok(Invocation::GenerateHelp),
            "--check" if inline.is_none() => fields.set_check()?,
            "--check" => return Err(UsageError::UnexpectedValue { flag: "--check" }),
            _ => take_path(&mut fields, flag, inline, arguments, &mut index)?,
        }
    }
    fields.finish().map(Invocation::Generate)
}

/// Records one path flag, taking its value from wherever it was written.
fn take_path(
    fields: &mut Fields,
    flag: &str,
    inline: Option<&str>,
    arguments: &[&str],
    index: &mut usize,
) -> Result<(), UsageError> {
    // Re-derived as a `&'static str` rather than carried over from argv, so a
    // diagnostic can only ever name a flag this parser defines.
    let flag = PATH_FLAGS
        .into_iter()
        .find(|known| *known == flag)
        .ok_or_else(|| UsageError::UnknownFlag {
            command: "generate",
            found: flag.to_string(),
        })?;
    let value = match inline {
        Some(value) => value,
        None => take_value(flag, arguments, index)?,
    };
    fields.set_path(flag, value)
}

/// `argument` as a flag and, if it was written `--flag=value`, its value.
///
/// `None` for anything that is not flag-shaped. A lone `-` is a bare word: no
/// flag here is spelled that way, and treating it as one would produce
/// "`generate` has no - flag" for what is almost certainly a mistyped path.
fn split_flag(argument: &str) -> Option<(&str, Option<&str>)> {
    if !argument.starts_with('-') || argument == "-" {
        return None;
    }
    match argument.split_once('=') {
        Some((flag, value)) => Some((flag, Some(value))),
        None => Some((argument, None)),
    }
}

/// The argument after a flag, advancing past it.
fn take_value<'a>(
    flag: &'static str,
    arguments: &[&'a str],
    index: &mut usize,
) -> Result<&'a str, UsageError> {
    let Some(value) = arguments.get(*index) else {
        return Err(UsageError::MissingValue { flag });
    };
    *index += 1;
    if value.starts_with("--") {
        return Err(UsageError::FlagShapedValue {
            flag,
            found: (*value).to_string(),
        });
    }
    Ok(value)
}

/// The flags seen so far, each at most once.
#[derive(Default)]
struct Fields {
    schema: Option<PathBuf>,
    dart: Option<PathBuf>,
    ts: Option<PathBuf>,
    check: bool,
}

impl Fields {
    /// Records `--check`, refusing a repeat.
    fn set_check(&mut self) -> Result<(), UsageError> {
        if self.check {
            return Err(UsageError::RepeatedFlag { flag: "--check" });
        }
        self.check = true;
        Ok(())
    }

    /// Records a path flag, refusing a repeat.
    fn set_path(&mut self, flag: &'static str, value: &str) -> Result<(), UsageError> {
        let slot = match flag {
            "--schema" => &mut self.schema,
            "--dart" => &mut self.dart,
            // Unreachable: the caller matches this same set.
            _ => &mut self.ts,
        };
        if slot.is_some() {
            return Err(UsageError::RepeatedFlag { flag });
        }
        *slot = Some(PathBuf::from(value));
        Ok(())
    }

    /// The request, if enough of it was given.
    fn finish(self) -> Result<Generate, UsageError> {
        let Fields {
            schema,
            dart,
            ts,
            check,
        } = self;
        let schema = schema.ok_or(UsageError::NoSchema)?;
        if dart.is_none() && ts.is_none() {
            return Err(UsageError::NoOutput);
        }
        Ok(Generate {
            schema,
            dart,
            ts,
            check,
        })
    }
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
