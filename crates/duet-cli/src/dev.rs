//! `duet dev`: run the host, watch the Dart, hot-reload on save.
//!
//! # Why this runs the host as a child process
//!
//! It is the shape the spec's §8.2 diagram already describes, and it is what
//! makes the VM service URI readable without touching a file descriptor.
//!
//! The Flutter engine prints `The Dart VM service is listening on …` from
//! native code, straight to fd 1. Spike C, which hosted the engine *inside*
//! the reload driver, could only see that by `dup2`-ing its own stdout onto a
//! pipe — which then swallows everything else the process prints, forever,
//! including `duet-host-stdio`'s own protocol.
//!
//! Here the engine's fd 1 is the **child's** fd 1, an ordinary
//! [`Stdio::piped`] pipe. This process reads it, watches for the
//! announcement, and echoes every line through to its own stdout so the
//! developer still sees their app's output. Nothing is redirected, the VM
//! service keeps its authentication code, and a host that also speaks a
//! protocol on stdout is unaffected — because `duet dev` never asks it to give
//! up its own stdout, only to be started by something that is listening.
//!
//! # The loop
//!
//! ```text
//! start the host ─> read its stdout ─> the VM service URI
//!                          │                    │
//!                          v                    v
//!                   echo to stdout      frontend_server + reloadSources
//!                                               ^
//!                                               │
//!                          a .dart file changes ┘
//! ```
//!
//! It ends when the host exits, or when the developer interrupts it. Either
//! way the child is killed and the compiler is reaped — see this module's `Session`
//! and its `Drop`.
//!
//! # It streams, so it does not return its output
//!
//! Every other command here buffers into [`crate::Outcome`] and lets the
//! caller decide what to do with it. This one cannot: a `duet dev` session
//! runs until it is interrupted, and buffering would mean the developer sees
//! nothing at all until they press Ctrl-C. So it writes as it goes, to writers
//! the caller supplies — which keeps it testable — and returns only the exit
//! code.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use duet_dev::{
    Announcement, CompileOutcome, DevError, DriverConfig, PackageConfig, Reload, ReloadDriver,
    Stage, Timeouts, VmServiceUrl, WatchConfig, Watcher,
};

use crate::args::Dev;

/// How long to wait for the host to announce its VM service.
///
/// Generous, because it covers whatever the host command actually does before
/// booting an engine — for `cargo run`, that includes compiling the host. A
/// clean debug build of this workspace's own examples takes well under a
/// minute; a cold one with dependencies can take several.
const ANNOUNCEMENT_TIMEOUT: Duration = Duration::from_secs(300);

/// How often the watcher rescans.
///
/// 60 ms against a debounce of 120 ms, so a save is noticed within one
/// debounce period rather than one poll period after it.
const POLL_INTERVAL: Duration = Duration::from_millis(60);

/// The child host process, killed on the way out.
///
/// A `duet dev` that returned while leaving a Flutter engine running would
/// leak a few hundred megabytes and a window every time the developer's
/// project failed to compile.
struct Session {
    child: Child,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs a `duet dev` session, writing progress to `out` and diagnostics to
/// `err`.
///
/// Returns the exit code: 0 if the host exited cleanly, [`crate::EXIT_FAILED`]
/// if the session could not be started or the host failed.
pub fn run(request: &Dev, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match start(request, out, err) {
        Ok(code) => code,
        Err(e) => {
            // Every `DevError` names the stage it happened at, so this is
            // always "[locate-sdk] no dartaotruntime at …" rather than a bare
            // sentence with no location.
            let _ = writeln!(err, "duet: {e}");
            let _ = writeln!(err, "\n{}", advice(&e));
            crate::EXIT_FAILED
        }
    }
}

/// The session proper, with every failure as a [`DevError`].
fn start(request: &Dev, out: &mut dyn Write, err: &mut dyn Write) -> Result<u8, DevError> {
    let flutter_root = resolve_flutter_root(
        request.flutter_root.as_deref(),
        std::env::var("FLUTTER_ROOT").ok().as_deref(),
    )?;
    let sdk = duet_dev::FlutterSdk::locate(&flutter_root)?;
    let package_config_path = duet_dev::package_config(&request.project)?;
    let packages = PackageConfig::read(&package_config_path)?;
    let entrypoint = match &request.entrypoint {
        Some(uri) => uri.clone(),
        None => packages.uri_for(&request.project.join("lib").join("main.dart")),
    };

    // The watcher is built before the host starts, so a mistyped project path
    // fails in a second rather than after a full `cargo run`.
    let mut watcher = Watcher::new(WatchConfig::dart_project(&request.project))?;

    let _ = writeln!(out, "duet dev");
    let _ = writeln!(out, "  project    {}", request.project.display());
    let _ = writeln!(out, "  entrypoint {entrypoint}");
    let _ = writeln!(
        out,
        "  watching   {} file(s) under {}",
        watcher.watched(),
        request.project.join("lib").display()
    );
    let _ = writeln!(out, "  host       {}", request.host.join(" "));
    let _ = writeln!(out);

    let (mut session, lines) = spawn_host(&request.host)?;
    let vm_service = await_announcement(&lines, out, ANNOUNCEMENT_TIMEOUT)?;
    let _ = writeln!(out, "[duet dev] VM service at {}", vm_service.websocket());

    let started = ReloadDriver::start(DriverConfig {
        compiler: duet_dev::CompilerConfig {
            sdk,
            package_config: package_config_path,
            output_dill: work_dir(&request.project).join("app.dill"),
        },
        entrypoint,
        vm_service,
        timeouts: Timeouts::default(),
    })?;
    report_compile(&started.baseline, out, err);
    let mut driver = started.driver;
    let _ = writeln!(
        out,
        "[duet dev] ready — save a .dart file under lib/ to hot-reload\n"
    );

    let code = loop {
        // The host exiting is what ends a session normally: the developer
        // closed the window.
        if let Ok(Some(status)) = session.child.try_wait() {
            let _ = writeln!(out, "\n[duet dev] the host exited ({status})");
            break u8::try_from(status.code().unwrap_or(0)).unwrap_or(1);
        }
        drain(&lines, out);

        if let Some(changed) = watcher.poll(Instant::now()) {
            let invalidated: Vec<String> =
                changed.iter().map(|path| packages.uri_for(path)).collect();
            let _ = writeln!(
                out,
                "[duet dev] {} file(s) changed, reloading…",
                changed.len()
            );
            match driver.reload(&invalidated) {
                Ok(outcome) => report_reload(&outcome, out, err),
                // A driver failure ends the session: the compiler is gone, or
                // the VM service is unreachable, and every subsequent save
                // would fail the same way. Better to stop and say so than to
                // print the same error on every keystroke.
                Err(e) => {
                    let _ = writeln!(err, "duet: {e}");
                    let _ = writeln!(err, "\n{}", advice(&e));
                    break crate::EXIT_FAILED;
                }
            }
        }
        thread::sleep(POLL_INTERVAL);
    };

    driver.shutdown();
    drain(&lines, out);
    Ok(code)
}

/// Starts the host with its stdout piped, and a thread reading it.
///
/// stderr is **inherited**, not piped: the developer's own `eprintln!` and any
/// panic message should reach the terminal immediately and in the right order
/// relative to everything else on stderr, and nothing here needs to read them.
fn spawn_host(command: &[String]) -> Result<(Session, Receiver<String>), DevError> {
    let Some((program, arguments)) = command.split_first() else {
        return Err(DevError::NotFound {
            stage: Stage::LocateVmService,
            what: "host command",
            path: "<empty>".to_string(),
        });
    };
    let mut child = Command::new(program)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|source| DevError::Io {
            stage: Stage::LocateVmService,
            doing: "starting the host process",
            source,
        })?;
    let stdout = child.stdout.take().ok_or_else(|| DevError::Io {
        stage: Stage::LocateVmService,
        doing: "taking the host's stdout",
        source: std::io::Error::other("the pipe was not created"),
    })?;

    let (sender, receiver) = channel();
    // A failure to spawn the reader leaves the channel unfed, which
    // `await_announcement` reports as a timeout — the right outcome, and one
    // that needs no separate error path.
    let _ = thread::Builder::new()
        .name("duet-dev-host-stdout".to_string())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
    Ok((Session { child }, receiver))
}

/// Reads the host's output until the VM service announcement appears, echoing
/// every line through.
///
/// Echoing is not a nicety. This process owns the pipe, so a line it swallowed
/// is a line the developer never sees — and during startup those lines are
/// exactly the ones that explain why the host is taking so long.
fn await_announcement(
    lines: &Receiver<String>,
    out: &mut dyn Write,
    timeout: Duration,
) -> Result<VmServiceUrl, DevError> {
    let scanner = Announcement;
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DevError::Timeout {
                stage: Stage::LocateVmService,
                after: timeout,
            });
        }
        match lines.recv_timeout(remaining) {
            Ok(line) => {
                let _ = writeln!(out, "{line}");
                if let Some(url) = scanner.read(&line) {
                    return Ok(url);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(DevError::Timeout {
                    stage: Stage::LocateVmService,
                    after: timeout,
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(DevError::NotFound {
                    stage: Stage::LocateVmService,
                    what: "VM service announcement (the host exited without printing one; \
                           is it a debug/JIT build?)",
                    path: "the host's stdout".to_string(),
                });
            }
        }
    }
}

/// Passes on whatever the host has printed since the last look, without
/// blocking.
fn drain(lines: &Receiver<String>, out: &mut dyn Write) {
    while let Ok(line) = lines.try_recv() {
        let _ = writeln!(out, "{line}");
    }
}

/// Reports a reload's outcome, one line for the good case and the compiler's
/// own diagnostics for the bad one.
fn report_reload(outcome: &Reload, out: &mut dyn Write, err: &mut dyn Write) {
    match outcome {
        Reload::Applied { report, timings } => {
            let _ = writeln!(
                out,
                "[duet dev] reloaded in {} ms (recompile {} ms, reload {} ms, reassemble {} ms){}\n",
                millis(timings.total),
                millis(timings.recompile),
                millis(timings.reload),
                millis(timings.reassemble),
                match (report.received_libraries, report.saved_libraries) {
                    (Some(received), Some(saved)) =>
                        format!(" — {received} librar(y/ies) reloaded, {saved} kept"),
                    _ => String::new(),
                }
            );
        }
        Reload::Declined { report, .. } => {
            let _ = writeln!(
                err,
                "duet: the Dart VM could not apply this change without restarting."
            );
            for notice in &report.notices {
                let _ = writeln!(err, "  {notice}");
            }
            let _ = writeln!(
                err,
                "  Restart the host to pick it up. (Hot reload cannot change a class's \
                 shape, an enum's values, or a const class's fields.)\n"
            );
        }
        Reload::CompileFailed {
            diagnostics,
            elapsed,
        } => {
            let _ = writeln!(
                err,
                "duet: the Dart did not compile ({} ms):",
                millis(*elapsed)
            );
            for line in diagnostics {
                let _ = writeln!(err, "  {line}");
            }
            let _ = writeln!(err, "  The host is still running; fix it and save again.\n");
        }
    }
}

/// Reports the one-off baseline compile.
///
/// A project that does not currently compile is a perfectly ordinary state to
/// start `duet dev` in, so this reports and continues rather than refusing to
/// start.
fn report_compile(baseline: &CompileOutcome, out: &mut dyn Write, err: &mut dyn Write) {
    if baseline.ok {
        let _ = writeln!(
            out,
            "[duet dev] baseline compile in {} ms",
            millis(baseline.elapsed)
        );
        return;
    }
    let _ = writeln!(err, "duet: the project does not currently compile:");
    for line in &baseline.diagnostics {
        let _ = writeln!(err, "  {line}");
    }
    let _ = writeln!(
        err,
        "  Watching anyway — fix it and save, and the next reload will pick it up.\n"
    );
}

/// Milliseconds, rounded, for a progress line.
fn millis(duration: Duration) -> u128 {
    duration.as_millis()
}

/// Picks the Flutter SDK: the flag if given, else `FLUTTER_ROOT`.
///
/// Takes the environment's value as an argument rather than reading it, so
/// this decision is testable without a test mutating process-global state that
/// every other test in the binary shares.
///
/// # Errors
///
/// [`DevError::NotFound`] naming both ways of supplying it, because a
/// developer hitting this has no way to know the environment variable is
/// consulted at all unless the message says so.
fn resolve_flutter_root(
    explicit: Option<&Path>,
    environment: Option<&str>,
) -> Result<PathBuf, DevError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    match environment.filter(|value| !value.trim().is_empty()) {
        Some(value) => Ok(PathBuf::from(value)),
        None => Err(DevError::NotFound {
            stage: Stage::LocateSdk,
            what: "Flutter SDK (pass --flutter-root, or set FLUTTER_ROOT)",
            path: "<not given>".to_string(),
        }),
    }
}

/// Where the incremental compiler's kernel output goes.
///
/// Under the project's `.dart_tool/`, which is already generated output and is
/// already on every Flutter `.gitignore` — and which
/// [`WatchConfig::dart_project`] never descends into, so the compiler's own
/// output cannot trigger the next reload.
fn work_dir(project: &Path) -> PathBuf {
    project.join(".dart_tool").join("duet_dev")
}

/// What to try next, chosen by the stage the failure happened at.
///
/// The stage is the whole reason this can say anything useful: the same
/// "could not reload" sentence would be worthless, but "it failed while
/// locating the SDK" and "it failed while talking to the Dart VM" have
/// completely different first moves.
fn advice(error: &DevError) -> &'static str {
    match error.stage() {
        Stage::LocateSdk => {
            "Point --flutter-root at a Flutter checkout, or set FLUTTER_ROOT. \
             If the path is right, `flutter precache` may not have run."
        }
        Stage::SpawnCompiler | Stage::BaselineCompile => {
            "The incremental compiler could not be started or primed. Check that \
             `flutter pub get` has run in the project."
        }
        Stage::LocateVmService => {
            "The host never announced a Dart VM service. It only exists in debug \
             and profile builds — a release/AOT host has none. Check the host \
             command actually boots a Flutter engine, and that it prints to stdout."
        }
        Stage::Connect | Stage::FindIsolate => {
            "The Dart VM service was announced but could not be driven. If the host \
             exited between the announcement and now, its own output above says why."
        }
        _ => {
            "Restart `duet dev`. If it repeats, the host or the compiler is in a \
              state this tool cannot recover from."
        }
    }
}

#[cfg(test)]
#[path = "dev_tests.rs"]
mod tests;
