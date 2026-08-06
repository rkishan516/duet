//! The reload cycle: recompile, apply, rebuild — with a deadline on every leg.
//!
//! This is Spike C's proven sequence
//! (`spikes/spike-c-macos/FINDINGS.md`) with the parts a throwaway spike could
//! skip: no `unwrap`, a stage on every error, a bound on every wait, and a
//! compile error modelled as an outcome rather than a failure.
//!
//! ```text
//! recompile ──> ok? ──no──> reject, report the diagnostics, done
//!                │
//!               yes
//!                v
//!             accept
//!                v
//!         reloadSources ──> success? ──no──> report the notices, done
//!                             │
//!                            yes
//!                             v
//!                  ext.flutter.reassemble
//! ```
//!
//! Two of those arrows are easy to get wrong and both are load-bearing:
//!
//! - **`reject` after a failed compile.** Without it the failed generation
//!   stays as the incremental baseline, so the *next* recompile diffs against
//!   broken code and produces a diff that does not describe the fix.
//! - **No `reassemble` after a declined reload.** Rebuilding the widget tree
//!   when no new code was loaded is at best pointless work on the UI thread,
//!   and at worst hides the decline behind a UI that visibly did something.

use std::time::{Duration, Instant};

use crate::error::DevError;
use crate::frontend_server::{CompileOutcome, CompilerConfig, FrontendServer, file_uri};
use crate::url::VmServiceUrl;
use crate::vm_service::{IsolateId, ReloadReport, VmServiceClient};

/// How long each leg of a cycle gets before it is declared hung.
///
/// Every one of these is far above what Spike C measured (9–22 ms recompile,
/// ~100 ms for the whole edit-to-frame path) because a deadline's job is to
/// turn a hang into a report, not to police performance. A first compile of a
/// large app is genuinely seconds — Spike C's took 3.8 s — so the baseline
/// gets its own, much longer, budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// The one-off full compile at startup.
    pub baseline: Duration,
    /// Opening the VM service WebSocket.
    pub connect: Duration,
    /// Any single JSON-RPC round trip.
    pub rpc: Duration,
    /// One incremental recompile.
    pub recompile: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            baseline: Duration::from_secs(120),
            connect: Duration::from_secs(20),
            rpc: Duration::from_secs(30),
            recompile: Duration::from_secs(60),
        }
    }
}

/// Where the time went in one cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timings {
    /// The incremental recompile.
    pub recompile: Duration,
    /// The `reloadSources` round trip.
    pub reload: Duration,
    /// The `ext.flutter.reassemble` round trip.
    pub reassemble: Duration,
    /// All of it, from entering [`ReloadDriver::reload`] to leaving it.
    ///
    /// Note what this does **not** include: the time between the developer
    /// saving the file and this being called, and the time between this
    /// returning and the change appearing on screen. Neither is observable
    /// from here. [`crate::Timings::total`] is the driver's cost; measuring
    /// edit-to-frame needs the guest to report a rendered frame, which is what
    /// `duet-backend-macos`'s `hot_reload` example does.
    pub total: Duration,
}

/// What one call to [`ReloadDriver::reload`] achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reload {
    /// New code is running.
    Applied {
        /// What the VM reported.
        report: ReloadReport,
        /// Where the time went.
        timings: Timings,
    },
    /// The Dart did not compile. **A normal event**, not a driver failure:
    /// the developer's next keystroke fixes it.
    CompileFailed {
        /// Every diagnostic line, untruncated.
        diagnostics: Vec<String>,
        /// How long the attempt took.
        elapsed: Duration,
    },
    /// The code compiled but the VM declined to apply it — a change hot reload
    /// cannot express, which needs a restart.
    Declined {
        /// The VM's report, including its `notices`.
        report: ReloadReport,
        /// Where the time went.
        timings: Timings,
    },
}

impl Reload {
    /// Whether new code is running.
    pub fn applied(&self) -> bool {
        matches!(self, Reload::Applied { .. })
    }
}

/// Everything needed to start a reload session.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// How to start the incremental compiler.
    pub compiler: CompilerConfig,
    /// The `package:` URI of the Dart entrypoint.
    pub entrypoint: String,
    /// Where the Dart VM service is.
    pub vm_service: VmServiceUrl,
    /// Per-leg deadlines.
    pub timeouts: Timeouts,
}

/// A started session, plus what its baseline compile produced.
///
/// The baseline is handed back rather than swallowed because it can fail on a
/// project that does not currently compile — which is a perfectly ordinary
/// state to start `duet dev` in. The session is still usable: the compiler is
/// running and the next edit produces a full recompile.
pub struct Started {
    /// The session.
    pub driver: ReloadDriver,
    /// What the one-off full compile produced.
    pub baseline: CompileOutcome,
}

/// A live hot-reload session: a compiler, a VM service, and an isolate.
pub struct ReloadDriver {
    compiler: FrontendServer,
    vm: VmServiceClient,
    isolate: IsolateId,
    entrypoint: String,
    timeouts: Timeouts,
}

impl ReloadDriver {
    /// Starts the compiler, primes it, and connects to the VM service.
    ///
    /// Ordered so the expensive, failure-prone thing happens first: the
    /// baseline compile takes seconds and is where a bad SDK path surfaces,
    /// and there is no point holding a VM service connection open through it.
    ///
    /// # Errors
    ///
    /// Any [`DevError`], each naming the stage it happened at:
    /// [`crate::Stage::SpawnCompiler`], [`crate::Stage::BaselineCompile`],
    /// [`crate::Stage::Connect`] or [`crate::Stage::FindIsolate`].
    pub fn start(config: DriverConfig) -> Result<Started, DevError> {
        let mut compiler = FrontendServer::spawn(&config.compiler)?;
        let baseline = compiler.compile(&config.entrypoint, config.timeouts.baseline)?;
        if baseline.ok {
            compiler.accept()?;
        } else {
            // Do not let a broken baseline become the diff base — see this
            // module's docs.
            compiler.reject()?;
        }

        let mut vm = VmServiceClient::connect(&config.vm_service, config.timeouts.connect)?;
        let isolate = vm.main_isolate(config.timeouts.rpc)?;

        Ok(Started {
            driver: ReloadDriver {
                compiler,
                vm,
                isolate,
                entrypoint: config.entrypoint,
                timeouts: config.timeouts,
            },
            baseline,
        })
    }

    /// The isolate this session reloads. For logging, and so a caller can tell
    /// whether it changed.
    pub fn isolate(&self) -> &IsolateId {
        &self.isolate
    }

    /// Runs one cycle for `invalidated`.
    ///
    /// An empty `invalidated` list falls back to the entrypoint, which is what
    /// a caller wanting "reload whatever changed, I do not know what" should
    /// pass — the compiler re-reads the whole import graph from there.
    ///
    /// # Errors
    ///
    /// Any [`DevError`]. Note the things that are **not** errors: a compile
    /// error is [`Reload::CompileFailed`] and a refused reload is
    /// [`Reload::Declined`], both `Ok`.
    pub fn reload(&mut self, invalidated: &[String]) -> Result<Reload, DevError> {
        let started = Instant::now();
        let fallback;
        let invalidated = if invalidated.is_empty() {
            fallback = [self.entrypoint.clone()];
            &fallback[..]
        } else {
            invalidated
        };

        let compiled = self
            .compiler
            .recompile(invalidated, self.timeouts.recompile)?;
        if !compiled.ok {
            self.compiler.reject()?;
            return Ok(Reload::CompileFailed {
                diagnostics: compiled.diagnostics,
                elapsed: started.elapsed(),
            });
        }
        self.compiler.accept()?;

        // The compiler tells us where it wrote the diff when it can; the
        // documented-nowhere `<output>.incremental.dill` is the fallback Spike
        // C found empirically. Preferring what it actually said means an SDK
        // that changes the location keeps working.
        let dill = match &compiled.dill {
            Some(path) => file_uri(std::path::Path::new(path)),
            None => file_uri(&self.compiler.incremental_dill()),
        };

        let reload_started = Instant::now();
        let report = self
            .vm
            .reload_sources(&self.isolate, &dill, self.timeouts.rpc)?;
        let reload_took = reload_started.elapsed();

        if !report.success {
            return Ok(Reload::Declined {
                report,
                timings: Timings {
                    recompile: compiled.elapsed,
                    reload: reload_took,
                    reassemble: Duration::ZERO,
                    total: started.elapsed(),
                },
            });
        }

        let reassemble_started = Instant::now();
        self.vm.reassemble(&self.isolate, self.timeouts.rpc)?;
        let reassemble_took = reassemble_started.elapsed();

        Ok(Reload::Applied {
            report,
            timings: Timings {
                recompile: compiled.elapsed,
                reload: reload_took,
                reassemble: reassemble_took,
                total: started.elapsed(),
            },
        })
    }

    /// Stops the compiler and closes the VM service connection.
    pub fn shutdown(self) {
        // `vm` drops first (sending a Close frame), then the compiler is asked
        // to quit — the reverse of start order.
        drop(self.vm);
        self.compiler.shutdown();
    }
}

#[cfg(test)]
#[path = "reload_tests.rs"]
mod tests;
