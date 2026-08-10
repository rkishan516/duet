//! Driving a long-lived `frontend_server` child process.
//!
//! The protocol itself is in `compiler_protocol`, with no I/O in it.
//! What is here is everything that makes a child process survivable:
//!
//! - **It can die.** Its stdout closing is an ordinary EOF, indistinguishable
//!   from a well-behaved end of stream unless you look. Every read
//!   distinguishes them and reports [`DevError::CompilerExited`] with the exit
//!   status and the tail of stderr, which is where the reason actually is.
//! - **It can hang.** `BufRead::read_line` on a pipe waits forever. Reads here
//!   go through a reader thread and a channel, so every one has a deadline and
//!   a stage.
//! - **It writes to stderr.** Spike C let it inherit the terminal, which
//!   scrambles a tool's own output and loses the text entirely when the tool
//!   is not attached to a terminal. Here it is captured into a bounded tail
//!   and surfaced in the error that needs it.
//! - **It can emit garbage.** A future SDK could change the line protocol.
//!   That surfaces as [`DevError::CompilerProtocol`], not as a hang.
//!
//! # A compile error is not a failure of this type
//!
//! [`FrontendServer::recompile`] returns `Ok(CompileOutcome)` when the
//! developer's Dart does not compile. That is the single most common thing
//! that happens in a hot-reload loop and it is not an error condition of the
//! driver — the driver worked perfectly, and its answer is "this does not
//! compile, here are the diagnostics". `Err` is reserved for the compiler
//! being broken, gone, or unintelligible.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::compiler_protocol::{
    self, Feed, ResultParser, Terminator, compile_command, recompile_command,
};
use crate::error::{DevError, Stage};
use crate::sdk::FlutterSdk;

/// How many stderr lines to keep for error reporting.
///
/// Bounded because a compiler stuck in a loop can produce stderr faster than
/// anything reads it, and an unbounded buffer in a long-lived dev session is a
/// leak. The tail is the useful end: a crash message is the last thing
/// written, not the first.
const STDERR_TAIL_LINES: usize = 40;

/// How many stdout lines may queue before the reader thread blocks.
///
/// Backpressure rather than an unbounded queue, for the same reason. Generous
/// enough that a compile emitting hundreds of diagnostics never stalls.
const STDOUT_QUEUE: usize = 4096;

/// What one `compile` or `recompile` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOutcome {
    /// Whether the sources compiled.
    ///
    /// From the terminator's error count when the server gives one, and from
    /// the diagnostics otherwise — see `compiler_protocol`.
    pub ok: bool,
    /// Every line the compiler wrote between the `result` and terminator
    /// lines, untruncated. This is what a developer needs to see.
    pub diagnostics: Vec<String>,
    /// How many errors the compiler reported, when it said.
    pub errors: Option<u32>,
    /// The dill the compiler wrote, when it said.
    pub dill: Option<String>,
    /// Wall time for the exchange.
    pub elapsed: Duration,
}

/// How to start the compiler.
#[derive(Debug, Clone)]
pub struct CompilerConfig {
    /// Resolved SDK artefacts.
    pub sdk: FlutterSdk,
    /// The project's `.dart_tool/package_config.json`.
    pub package_config: PathBuf,
    /// Where the baseline kernel is written. Its sibling
    /// `<output_dill>.incremental.dill` is what each `recompile` produces.
    pub output_dill: PathBuf,
}

/// A running `frontend_server`.
pub struct FrontendServer {
    child: Child,
    stdin: ChildStdin,
    /// Lines from the child's stdout, or the read error that ended them.
    lines: Receiver<Result<String, std::io::Error>>,
    /// The tail of the child's stderr, shared with its reader thread.
    stderr: Arc<Mutex<Vec<String>>>,
    /// The stderr reader thread, joined by [`FrontendServer::exited`] before
    /// the tail is read — see there for the race this closes.
    stderr_reader: Option<thread::JoinHandle<()>>,
    /// Whether this child has ever produced a line of stdout, across every
    /// exchange. This — not the site where death was noticed — is what
    /// decides the never-answered hint in [`FrontendServer::exited`]: a child
    /// that dies before the first command's *write* still never answered,
    /// and hard-coding the write path's answer lost exactly that case (a CI
    /// runner slow enough for an instantly-dying child to be dead before the
    /// write reported "died mid-conversation" for a compiler that never said
    /// a word).
    ever_answered: bool,
    output_dill: PathBuf,
    /// Monotonic source of boundary keys.
    boundary: u64,
}

impl FrontendServer {
    /// Starts the compiler and returns once the process exists.
    ///
    /// The flags mirror `flutter_tools`' own resident compiler
    /// (`packages/flutter_tools/lib/src/compile.dart`) for a debug build.
    /// `--no-print-incremental-dependencies` is load-bearing for *parsing*,
    /// not for performance: without it every compile also prints 150–600
    /// `+file://…` lines, which the protocol has no way to distinguish from
    /// diagnostics.
    ///
    /// # Errors
    ///
    /// [`DevError::Io`] at [`Stage::SpawnCompiler`] if the process could not
    /// be started or its pipes could not be taken.
    pub fn spawn(config: &CompilerConfig) -> Result<Self, DevError> {
        if let Some(parent) = config.output_dill.parent() {
            std::fs::create_dir_all(parent).map_err(|source| DevError::Io {
                stage: Stage::SpawnCompiler,
                doing: "creating the compiler's output directory",
                source,
            })?;
        }

        let mut child = Command::new(&config.sdk.dartaotruntime)
            .arg(&config.sdk.frontend_server)
            .arg("--sdk-root")
            .arg(config.sdk.sdk_root_argument())
            .arg("--incremental")
            .arg("--target=flutter")
            // Debug builds default `trackWidgetCreation` on, and the kernel
            // the engine booted from was built with it. A mismatch here means
            // the diff and the running program disagree about widget
            // constructors.
            .arg("--track-widget-creation")
            .arg("--experimental-emit-debug-metadata")
            .arg("--no-print-incremental-dependencies")
            .arg("--packages")
            .arg(&config.package_config)
            .arg("--output-dill")
            .arg(&config.output_dill)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| DevError::Io {
                stage: Stage::SpawnCompiler,
                doing: "starting frontend_server",
                source,
            })?;

        let take = |what: &'static str| DevError::Io {
            stage: Stage::SpawnCompiler,
            doing: what,
            source: std::io::Error::other("the pipe was not created"),
        };
        let stdin = child.stdin.take().ok_or_else(|| take("taking its stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| take("taking its stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| take("taking its stderr"))?;

        let (sender, lines) = sync_channel(STDOUT_QUEUE);
        spawn_reader("duet-dev-fs-stdout", stdout, sender);

        let tail = Arc::new(Mutex::new(Vec::new()));
        let stderr_reader = spawn_stderr_reader(stderr, Arc::clone(&tail));

        Ok(FrontendServer {
            child,
            stdin,
            lines,
            stderr: tail,
            stderr_reader,
            ever_answered: false,
            output_dill: config.output_dill.clone(),
            boundary: 0,
        })
    }

    /// The path each `recompile` writes its diff to.
    ///
    /// `<output-dill>.incremental.dill`, which is documented nowhere and was
    /// found by Spike C by looking in the working directory after a successful
    /// recompile. It is what [`crate::VmServiceClient::reload_sources`] is
    /// pointed at.
    pub fn incremental_dill(&self) -> PathBuf {
        let mut name = self.output_dill.clone().into_os_string();
        name.push(".incremental.dill");
        PathBuf::from(name)
    }

    /// The full compile that primes the incremental compiler.
    ///
    /// `entrypoint` should be a `package:` URI, not a `file://` path — that is
    /// what `flutter_tools` uses, and it is how the root library's identity in
    /// the diff matches how the already-running program identifies it.
    ///
    /// # Errors
    ///
    /// See `FrontendServer::exchange`.
    pub fn compile(
        &mut self,
        entrypoint: &str,
        timeout: Duration,
    ) -> Result<CompileOutcome, DevError> {
        self.exchange(
            Stage::BaselineCompile,
            &compile_command(entrypoint),
            timeout,
        )
    }

    /// An incremental recompile of `invalidated`.
    ///
    /// # Errors
    ///
    /// See `FrontendServer::exchange`.
    pub fn recompile(
        &mut self,
        invalidated: &[String],
        timeout: Duration,
    ) -> Result<CompileOutcome, DevError> {
        self.boundary = self.boundary.wrapping_add(1);
        let key = format!("duet-dev-{}", self.boundary);
        self.exchange(
            Stage::Recompile,
            &recompile_command(&key, invalidated),
            timeout,
        )
    }

    /// Commits the last compile as the baseline for the next diff.
    ///
    /// Has no reply of its own — the confirmation it sometimes emits arrives
    /// interleaved with the *next* command's output, which
    /// `compiler_protocol`'s result parser discards as stray. So this
    /// writes and returns rather than waiting for something that may never
    /// come.
    ///
    /// # Errors
    ///
    /// [`DevError::CompilerExited`] if the write failed because the child is
    /// gone.
    pub fn accept(&mut self) -> Result<(), DevError> {
        self.write(Stage::Accept, "accept\n")
    }

    /// Sends `reject`, discarding the last compile.
    ///
    /// Used after a failed compile so the next `recompile` still diffs against
    /// the last *good* generation. Without it, a broken edit followed by a fix
    /// produces a diff relative to the broken state.
    ///
    /// # Errors
    ///
    /// [`DevError::CompilerExited`] if the child is gone.
    pub fn reject(&mut self) -> Result<(), DevError> {
        self.write(Stage::Accept, "reject\n")
    }

    /// Writes one command, then reads one result block.
    ///
    /// # Errors
    ///
    /// [`DevError::CompilerExited`] if the child died or its stdout closed
    /// mid-block, [`DevError::Timeout`] at `stage` if the block did not
    /// complete in `timeout`, [`DevError::CompilerProtocol`] if the child
    /// never produced a `result` line, and [`DevError::Io`] on a read error.
    fn exchange(
        &mut self,
        stage: Stage,
        command: &str,
        timeout: Duration,
    ) -> Result<CompileOutcome, DevError> {
        let started = Instant::now();
        self.write(stage, command)?;

        let deadline = started + timeout;
        let mut parser = ResultParser::default();
        let mut seen = 0usize;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(stalled(stage, timeout, &parser, seen));
            }
            match self.lines.recv_timeout(remaining) {
                Ok(Ok(line)) => {
                    self.ever_answered = true;
                    seen += 1;
                    if let Feed::Done(terminator) = parser.feed(&line) {
                        return Ok(self.finish(parser.take_diagnostics(), terminator, started));
                    }
                }
                Ok(Err(source)) => {
                    return Err(DevError::Io {
                        stage,
                        doing: "reading frontend_server's stdout",
                        source,
                    });
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(stalled(stage, timeout, &parser, seen));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // The reader thread ended, which means stdout hit EOF —
                    // the child is gone, or closed the pipe. Either way the
                    // developer needs its stderr, not "unexpected EOF".
                    return Err(self.exited(stage));
                }
            }
        }
    }

    /// Builds a [`CompileOutcome`] from a completed block.
    ///
    /// `ok` prefers the compiler's own error count and falls back to reading
    /// the diagnostics — see [`crate::compiler_protocol::is_error`] for why
    /// the fallback is deliberately narrow.
    fn finish(
        &self,
        diagnostics: Vec<String>,
        terminator: Terminator,
        started: Instant,
    ) -> CompileOutcome {
        let ok = match terminator.errors {
            Some(count) => count == 0,
            None => !diagnostics
                .iter()
                .any(|line| compiler_protocol::is_error(line)),
        };
        CompileOutcome {
            ok,
            diagnostics,
            errors: terminator.errors,
            dill: terminator.dill,
            elapsed: started.elapsed(),
        }
    }

    /// Writes a command to the child, mapping a broken pipe to
    /// [`DevError::CompilerExited`].
    fn write(&mut self, stage: Stage, command: &str) -> Result<(), DevError> {
        match self
            .stdin
            .write_all(command.as_bytes())
            .and_then(|()| self.stdin.flush())
        {
            Ok(()) => Ok(()),
            // A dead child's pipe reports `BrokenPipe`; anything else is a
            // genuine I/O fault worth reporting as itself. Whether the child
            // ever answered is `ever_answered`'s call, not this site's: a
            // child that died before the session's first write never spoke,
            // and claiming otherwise here suppressed the never-answered hint
            // exactly when it applied.
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(self.exited(stage)),
            Err(source) => Err(DevError::Io {
                stage,
                doing: "writing to frontend_server's stdin",
                source,
            }),
        }
    }

    /// Builds the "the compiler is gone" error, collecting its status and the
    /// tail of its stderr.
    ///
    /// `self.ever_answered` distinguishes "died mid-conversation" from "never
    /// started talking", which is usually a bad `--sdk-root` or a snapshot
    /// from a different SDK — a different first thing to check.
    fn exited(&mut self, stage: Stage) -> DevError {
        // Give a just-exited child a moment to be reapable, so the status is
        // usually present rather than usually absent.
        let status = match self.child.wait() {
            Ok(status) => status.code(),
            Err(_) => None,
        };
        // `wait` proves the child flushed its last stderr bytes into the pipe;
        // it says nothing about whether the reader thread has *read* them yet.
        // On a loaded machine the error was being built here with an empty
        // tail while the reason sat unread in the pipe — a third CI run lost
        // exactly that race. The child is dead, so the pipe is at EOF and the
        // reader exits after draining it: this join is bounded, and after it
        // the tail holds everything the child ever said.
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        let mut stderr = match self.stderr.lock() {
            Ok(tail) => tail.join("\n"),
            // A poisoned lock means the stderr reader panicked. Losing the
            // tail is survivable; failing to report the death is not.
            Err(_) => "<the stderr reader failed>".to_string(),
        };
        if !self.ever_answered {
            stderr.push_str(
                "\n(it exited before answering at all, which usually means a bad --sdk-root \
                 or a frontend_server snapshot from a different SDK)",
            );
        }
        DevError::CompilerExited {
            stage,
            status,
            stderr: crate::error::truncate(stderr.trim()),
        }
    }

    /// Asks the compiler to quit, then makes sure it is gone.
    ///
    /// Best effort throughout: this runs on the way out, and every failure
    /// here has the same remedy, which `kill` already is.
    pub fn shutdown(mut self) {
        let _ = self.stdin.write_all(b"quit\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for FrontendServer {
    /// Reaps the child even if [`FrontendServer::shutdown`] was never called —
    /// on an early `?` return, say. Without this, a `duet dev` session that
    /// failed to start would leave an orphaned compiler holding a few hundred
    /// megabytes.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Decides what an exchange that ran out of time actually means.
///
/// A compiler that has said **nothing** is hung, and a deadline is the right
/// report. A compiler that has said several things but never opened a result
/// block is not hung at all — it is talking a protocol this crate does not
/// recognise, which is what a Flutter upgrade that changed the line format
/// would look like. Those have completely different fixes, and calling the
/// second one a timeout would send a developer hunting for a wedged process
/// that is working perfectly well.
fn stalled(stage: Stage, timeout: Duration, parser: &ResultParser, lines: usize) -> DevError {
    if lines > 0 && !parser.started() {
        return DevError::protocol(
            stage,
            format!(
                "it wrote {lines} line(s) in {timeout:?} but never a `result <boundary>` line, \
                 so this crate could not tell where its answer began — the frontend_server \
                 line protocol may have changed in this Flutter SDK"
            ),
        );
    }
    DevError::Timeout {
        stage,
        after: timeout,
    }
}

/// Reads `source` line by line onto `sender` until EOF.
///
/// A thread rather than non-blocking I/O because that is the only portable way
/// to put a deadline on a pipe read: `recv_timeout` on the other end does what
/// `read_line` cannot.
fn spawn_reader<R: std::io::Read + Send + 'static>(
    name: &str,
    source: R,
    sender: SyncSender<Result<String, std::io::Error>>,
) {
    let builder = thread::Builder::new().name(name.to_string());
    // If the thread cannot be spawned the channel is simply never fed, and the
    // first read reports `Disconnected` — which is already handled as "the
    // compiler is gone". That is the right outcome, so there is nothing to
    // report here and nothing to unwrap.
    let _ = builder.spawn(move || {
        let reader = BufReader::new(source);
        for line in reader.lines() {
            // A receiver that has gone away means the driver moved on; stop
            // rather than block forever on a full channel.
            if sender.send(line).is_err() {
                return;
            }
        }
    });
}

/// Keeps the last [`STDERR_TAIL_LINES`] lines of the child's stderr.
///
/// Returns the reader's handle so [`FrontendServer::exited`] can join it
/// before reading the tail; `None` only if the thread could not be spawned at
/// all, in which case there is nothing to wait for and the tail simply stays
/// empty.
fn spawn_stderr_reader<R: std::io::Read + Send + 'static>(
    source: R,
    tail: Arc<Mutex<Vec<String>>>,
) -> Option<thread::JoinHandle<()>> {
    let builder = thread::Builder::new().name("duet-dev-fs-stderr".to_string());
    builder
        .spawn(move || {
            let reader = BufReader::new(source);
            for line in reader.lines() {
                let Ok(line) = line else { return };
                let Ok(mut tail) = tail.lock() else { return };
                if tail.len() == STDERR_TAIL_LINES {
                    tail.remove(0);
                }
                tail.push(line);
            }
        })
        .ok()
}

/// A `file://` URI for `path`, which is the only form `reloadSources` accepts
/// for `rootLibUri`.
///
/// Absolute paths only, because a relative one would be resolved against the
/// *Dart VM's* working directory rather than this process's — a difference
/// that shows up as a reload that reports success against a stale kernel.
///
/// A POSIX absolute path starts with `/`, so gluing it after `file://`
/// produces the well-formed empty-authority form (`file:///tmp/app.dill`). A
/// Windows drive path does not — `file://C:\...` puts the drive letter where
/// a URI host goes, and carries separators a URI may not — so it gets the
/// missing slash added and its separators flipped (`file:///C:/app.dill`).
/// Found by the Windows `hot_reload` example: the malformed form made every
/// `reloadSources` decline with no stated reason.
pub fn file_uri(path: &Path) -> String {
    let display = path.display().to_string();
    if display.starts_with('/') {
        format!("file://{display}")
    } else {
        format!("file:///{}", display.replace('\\', "/"))
    }
}

#[cfg(test)]
#[path = "frontend_server_tests.rs"]
mod tests;
