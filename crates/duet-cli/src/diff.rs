//! A bounded line diff, so `--check` says *what* differs.
//!
//! # Why a diff rather than "the files differ"
//!
//! `--check` fails on a machine the developer is not sitting at. "`app.duet.ts`
//! is out of date" leaves them to guess whether a field was renamed, a type
//! changed, or only the header moved — and the usual next step, running the
//! generator locally and diffing, is exactly the work the message could have
//! done. Naming the line turns a CI failure into a review comment.
//!
//! # Why it is bounded
//!
//! Both sides are derived from a schema file this tool does not control. An
//! unbounded diff of a hostile schema's output is a way to flood whatever
//! collects CI logs, and a stale golden with a one-line header change would
//! otherwise print a thousand identical-looking `+`/`-` pairs anyway. So: one
//! difference, in context, with a count of the rest.

use std::fmt::Write as _;

/// Lines of matching context printed before the first difference.
pub const CONTEXT_LINES: usize = 3;

/// The most characters of one line the diff will echo.
pub const MAX_LINE_CHARS: usize = 120;

/// The first difference between `committed` and `generated`, in context.
///
/// Empty when the two are identical, so a caller can use emptiness as the
/// comparison — one notion of "the same" rather than two that could disagree.
pub fn render(committed: &str, generated: &str) -> String {
    if committed == generated {
        return String::new();
    }
    let (committed_text, generated_text) = (committed, generated);
    let committed: Vec<&str> = committed_text.lines().collect();
    let generated: Vec<&str> = generated_text.lines().collect();
    let first = first_difference(&committed, &generated);
    if first >= committed.len() && first >= generated.len() {
        // Every line matched and the lengths agree, so the two can only differ
        // in the terminator `lines()` discarded. Saying "(end of file)" twice
        // would be true and unreadable.
        return only_the_terminator(committed_text, generated_text);
    }
    let mut out = String::new();
    for offset in first.saturating_sub(CONTEXT_LINES)..first {
        write_line(&mut out, ' ', offset, committed.get(offset).copied());
    }
    write_line(&mut out, '-', first, committed.get(first).copied());
    write_line(&mut out, '+', first, generated.get(first).copied());
    write_remainder(&mut out, &committed, &generated, first);
    out
}

/// The report for two texts whose lines all match.
///
/// Every generated file ends in exactly one newline — `duet-codegen`'s golden
/// tests assert it — so a file reaching this arm has had its terminator eaten
/// by an editor or a copy, and naming that is far more useful than pointing at
/// a line.
fn only_the_terminator(committed: &str, generated: &str) -> String {
    let describe = |text: &str| {
        if text.ends_with('\n') {
            "ends with a newline"
        } else {
            "does not end with a newline"
        }
    };
    format!(
        "  every line matches; the file on disk {}, the generated text {}.\n",
        describe(committed),
        describe(generated)
    )
}

/// The index of the first line the two sides disagree on.
///
/// When one side is a prefix of the other, that is its length: the first line
/// the shorter side does not have.
fn first_difference(committed: &[&str], generated: &[&str]) -> usize {
    committed
        .iter()
        .zip(generated)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| committed.len().min(generated.len()))
}

/// One `<marker> <line number> | <text>` row.
///
/// A line number on both sides of the pair rather than only the first, because
/// the numbers are equal here by construction and a reader should not have to
/// know that to trust them.
fn write_line(out: &mut String, marker: char, index: usize, line: Option<&str>) {
    let number = index + 1;
    match line {
        Some(text) => {
            let _ = writeln!(out, "{marker} {number:>5} | {}", truncate(text));
        }
        None => {
            let _ = writeln!(out, "{marker} {number:>5} | (end of file)");
        }
    }
}

/// How much else differs, so the reader knows whether they have seen it all.
fn write_remainder(out: &mut String, committed: &[&str], generated: &[&str], first: usize) {
    let further = committed
        .iter()
        .zip(generated)
        .skip(first + 1)
        .filter(|(a, b)| a != b)
        .count();
    let (shorter, longer) = (committed.len().min(generated.len()), {
        committed.len().max(generated.len())
    });
    let extra = longer - shorter;
    let mut notes = Vec::new();
    if further > 0 {
        notes.push(format!("{further} more line(s) differ"));
    }
    if extra > 0 {
        let which = if generated.len() > committed.len() {
            "would be added"
        } else {
            "would be removed"
        };
        notes.push(format!("{extra} line(s) {which}"));
    }
    if !notes.is_empty() {
        let _ = writeln!(out, "  … and {}.", notes.join(", "));
    }
}

/// `text`, cut to [`MAX_LINE_CHARS`] characters on a character boundary.
fn truncate(text: &str) -> String {
    match text.char_indices().nth(MAX_LINE_CHARS) {
        None => text.to_string(),
        Some((at, _)) => format!("{}…", &text[..at]),
    }
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
