//! Turning a [`CliError`] into the bytes a human reads.
//!
//! Kept apart from [`crate::error`] because the two answer different questions.
//! `Display` on an error answers "what is wrong", in one line, for a log. This
//! answers "what do I do now", in as many lines as that takes, for a person
//! staring at a red CI job.

use std::fmt::Write as _;

use crate::error::{CliError, Difference};

/// The whole stderr output for a failed run.
///
/// `command` is the invocation that would fix a `--check` failure — the same
/// string the generated files carry in their headers, so a developer who
/// reaches for either finds the same thing to run.
pub fn failure(error: &CliError, command: Option<&str>) -> String {
    match error {
        CliError::Stale(differences) => stale(differences, command),
        CliError::Usage(_) => {
            format!("duet: {error}\n\nRun `duet --help` to see what this command takes.\n")
        }
        _ => format!("duet: {error}\n"),
    }
}

/// The per-file `--check` report.
///
/// Names every file that differs rather than stopping at the first, because a
/// schema change usually moves both clients and fixing them one CI run at a
/// time is a waste of the developer's afternoon.
fn stale(differences: &[Difference], command: Option<&str>) -> String {
    let mut out = format!(
        "duet: {} generated file(s) are out of date\n",
        differences.len()
    );
    for difference in differences {
        out.push('\n');
        if difference.missing {
            let _ = writeln!(out, "{}: does not exist yet", difference.path.display());
        } else {
            let _ = writeln!(out, "{}:", difference.path.display());
            out.push_str(&difference.diff);
        }
    }
    if let Some(command) = command {
        let _ = write!(out, "\nRegenerate with:\n    {command}\n");
    }
    out
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
