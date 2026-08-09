//! Resident set size, sampled from the outside.
//!
//! The showcase's fourth claim — "tear a guest down and the memory comes back" —
//! is the one Duet exists for, and the only honest way to show it is to measure
//! the process the way an operating system does. `ps` is what
//! `crates/duet-backend-macos/examples/lifecycle.rs` uses for the same reason:
//! it is the number a user would see in Activity Monitor, not a number this
//! process computed about itself.

use std::process::Command;

/// One labelled RSS reading.
#[derive(Debug, Clone)]
pub struct Sample {
    /// What the process was doing when this was taken.
    pub label: &'static str,
    /// Resident set size in kilobytes, or `None` if `ps` could not be read.
    pub kb: Option<i64>,
}

impl Sample {
    /// Takes a reading now.
    pub fn take(label: &'static str) -> Sample {
        Sample {
            label,
            kb: rss_kb(),
        }
    }

    /// This sample minus `earlier`, when both were readable.
    pub fn minus(&self, earlier: &Sample) -> Option<i64> {
        Some(self.kb? - earlier.kb?)
    }

    /// The reading, rendered for a table.
    pub fn rendered(&self) -> String {
        match self.kb {
            Some(kb) => format!("{kb} kB"),
            None => "unreadable".to_string(),
        }
    }
}

/// This process's resident set size in kilobytes.
///
/// Returns `None` rather than a sentinel when `ps` is unavailable or its output
/// does not parse: a demo that reported `-1 kB` as a measurement would be worse
/// than one that says it could not measure.
fn rss_kb() -> Option<i64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}
