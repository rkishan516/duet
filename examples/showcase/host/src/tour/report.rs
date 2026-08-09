//! What the showcase claims, and whether it saw it.
//!
//! Nobody can watch this run. There is no reachable on-screen WindowServer for a
//! spawned process on the machine this was written on: both windows are created
//! and both renderers draw into them, but no display shows either one. So the
//! demo has to be its own witness — every claim below is recorded with the
//! evidence that settled it, printed at the end, and the process exits non-zero
//! if any of them failed or was never reached.

use crate::tour::rss::Sample;

/// How many claims a complete run records. A run that records fewer stopped
/// early, and the report says so rather than printing a shorter list of passes.
pub const EXPECTED_CLAIMS: usize = 10;

/// One claim, and what settled it.
#[derive(Debug, Clone)]
pub struct Claim {
    /// A short name, printed as the headline.
    pub name: &'static str,
    /// Whether the evidence matched.
    pub passed: bool,
    /// The evidence, in full. Always the values actually observed, never a
    /// restatement of the claim.
    pub detail: String,
}

/// Every claim, in the order the tour reached them.
#[derive(Debug, Default)]
pub struct Results {
    claims: Vec<Claim>,
}

impl Results {
    /// Records one claim and prints it as it happens, so a run that later hangs
    /// still shows how far it got.
    pub fn record(&mut self, name: &'static str, passed: bool, detail: impl Into<String>) {
        let claim = Claim {
            name,
            passed,
            detail: detail.into(),
        };
        println!(
            "  {} {}: {}",
            if claim.passed { "PASS" } else { "FAIL" },
            claim.name,
            claim.detail
        );
        self.claims.push(claim);
    }

    /// Prints the summary and returns the process exit code.
    pub fn print(&self, samples: &[Sample], timed_out_at: Option<&'static str>) -> i32 {
        println!();
        println!("=== resident set size ===");
        let first = samples.first();
        for sample in samples {
            let delta = first
                .and_then(|base| sample.minus(base))
                .map_or_else(String::new, |d| {
                    format!("   ({d:+} kB from the first sample)")
                });
            println!("  {:<34} {:>12}{delta}", sample.label, sample.rendered());
        }

        println!();
        println!("=== claims ===");
        for claim in &self.claims {
            println!(
                "  {} {}",
                if claim.passed { "PASS" } else { "FAIL" },
                claim.name
            );
        }

        let failed = self.claims.iter().filter(|c| !c.passed).count();
        let missing = EXPECTED_CLAIMS.saturating_sub(self.claims.len());
        println!();
        if let Some(step) = timed_out_at {
            println!("TIMED OUT at {step}");
        }
        println!(
            "{} of {EXPECTED_CLAIMS} claims recorded, {} passed, {failed} failed, {missing} never \
             reached",
            self.claims.len(),
            self.claims.len() - failed
        );

        if failed == 0 && missing == 0 && timed_out_at.is_none() {
            println!("OK");
            0
        } else {
            println!("FAILED");
            1
        }
    }
}
