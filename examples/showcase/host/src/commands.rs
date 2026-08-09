//! The host commands both guests invoke.
//!
//! A `#[command]` is an ordinary Rust function. The attribute leaves it callable
//! from Rust and additionally describes it to the schema, so the same two
//! functions below become `appendLine(...)`/`wordCount(...)` in the generated
//! Dart client and the generated TypeScript client with no second definition
//! anywhere.
//!
//! # Why these two
//!
//! [`append_line`] takes `&CommandContext`, so it can read and write the store:
//! it is the only writer of `document.lines`, which is what lets two guests
//! append concurrently without either clobbering the other. It returns
//! `Result<i64, ComposeError>`, so a guest sees `returned` on success and
//! `raised` — carrying a *typed* domain error — on a blank line. Both arms are
//! exercised by both guests on every run.
//!
//! [`word_count`] takes no context and cannot fail. It is here to show the other
//! end of the range: a plain `fn(String) -> i64` is already a command.
//!
//! # What a raise is not
//!
//! `Err(ComposeError)` is the command **running** and reporting a domain
//! outcome. It arrives at a guest as `raised`, and the generated client decodes
//! it into a typed `ComposeError`. A host *refusal* — no such command, an
//! argument that would not decode — is a different thing entirely: it arrives as
//! `failed` and surfaces as an exception in the guest, never as an `Err`. A
//! `#[command]` body cannot produce one.

use duet::{CommandContext, CommandEntry, Path, Value, command, commands};

use crate::state::ComposeError;

/// Where [`append_line`] appends. The one path in this crate that is written by
/// hand — a command body addresses the store directly, and there is no
/// generated Rust accessor for it to borrow.
pub const LINES_PATH: &str = "document.lines";

/// The longest line [`append_line`] will accept.
pub const MAX_LINE_CHARS: usize = 120;

/// Appends one line to `document.lines` and returns the new line count.
///
/// # Errors
///
/// Raises [`ComposeError`] with code `empty_line` for a blank line,
/// `line_too_long` past [`MAX_LINE_CHARS`], and `store` if the store refuses the
/// read or the write.
#[command]
pub fn append_line(ctx: &CommandContext, text: String) -> Result<i64, ComposeError> {
    if text.trim().is_empty() {
        return Err(ComposeError::new(
            "empty_line",
            "a line needs at least one non-blank character",
        ));
    }
    if text.chars().count() > MAX_LINE_CHARS {
        return Err(ComposeError::new(
            "line_too_long",
            format!("a line may be at most {MAX_LINE_CHARS} characters"),
        ));
    }

    let path = Path::parse(LINES_PATH).map_err(|e| {
        ComposeError::new("store", format!("{LINES_PATH} is not a valid path: {e}"))
    })?;
    let store = ctx.store();
    let existing = store
        .get(&path)
        .map_err(|e| ComposeError::new("store", format!("reading {LINES_PATH} failed: {e}")))?;

    // Absent is treated as empty rather than as an error: a host that seeded the
    // store from `initial_state` always has the key, but a command should not
    // depend on the seeding of a store it does not own.
    let mut lines = match existing {
        Some(Value::List(items)) => items,
        None | Some(Value::Null) => Vec::new(),
        Some(other) => {
            return Err(ComposeError::new(
                "store",
                format!("{LINES_PATH} holds {other:?}, which is not a list"),
            ));
        }
    };
    lines.push(Value::Str(text));

    let count = i64::try_from(lines.len()).map_err(|_| {
        ComposeError::new("store", "the document has more lines than an i64 can count")
    })?;
    store
        .set(&path, Value::List(lines))
        .map_err(|e| ComposeError::new("store", format!("writing {LINES_PATH} failed: {e}")))?;
    Ok(count)
}

/// Counts whitespace-separated words. Pure: no context, no failure mode.
#[command]
pub fn word_count(text: String) -> i64 {
    // `saturating` rather than a cast: `split_whitespace().count()` is a `usize`
    // and this must be total, even on a string no machine could hold.
    i64::try_from(text.split_whitespace().count()).unwrap_or(i64::MAX)
}

/// The table a surface is built with.
///
/// This is the authorization boundary, not a convenience: a surface can only
/// invoke what is in the slice it was handed. Both guests here get the same
/// table, but two surfaces could just as easily get two different ones over one
/// store.
pub static COMMANDS: [CommandEntry; 2] = commands![append_line, word_count];
