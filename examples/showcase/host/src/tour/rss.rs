//! Resident set size, as the operating system accounts it.
//!
//! The showcase's fourth claim — "tear a guest down and the memory comes back" —
//! is the one Duet exists for, and the only honest way to show it is to measure
//! the process the way an operating system does. On macOS that is `ps`, the
//! number a user would see in Activity Monitor, read from outside the process —
//! the same choice `crates/duet-backend-macos/examples/lifecycle.rs` makes. On
//! Windows there is no `ps`; the reading is `K32GetProcessMemoryInfo`'s
//! working-set size — the number Task Manager shows — queried in-process but
//! still the OS's own accounting rather than anything this process computed
//! about itself, and the same source the Windows backend's own lifecycle
//! example measures with.

#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
fn rss_kb() -> Option<i64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// This process's working-set size in kilobytes — what Task Manager reports.
///
/// Returns `None` rather than a sentinel if the query fails, for the reason
/// the macOS arm gives: a demo that reported `-1 kB` as a measurement would be
/// worse than one that says it could not measure.
#[cfg(target_os = "windows")]
fn rss_kb() -> Option<i64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: the struct is plain data sized-and-zeroed here, the pseudo-handle
    // from GetCurrentProcess is always valid in-process, and the API writes at
    // most `cb` bytes.
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) == 0 {
            return None;
        }
        i64::try_from(counters.WorkingSetSize / 1024).ok()
    }
}
