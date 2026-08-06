//! Finding the Dart VM service, without redirecting anybody's stdout.
//!
//! # The problem this module exists to retire
//!
//! Spike C recovered the VM service URI by `dup2`-ing this process's fd 1 onto
//! a pipe and scanning it (`spikes/spike-c-macos/src/stdout_capture.rs`). The
//! engine prints the URI from native code with a raw write to fd 1, and Spike
//! C hosted the engine *inside the same process* as the reload driver, so
//! there was no other way to see it.
//!
//! That hack has a real cost, which Spike C's own module docs state: fd 1 is
//! fd 1. Once redirected, **everything** the process writes to stdout
//! disappears into the same pipe — the host's own `println!`, any library's,
//! any `dbg!`. Spike C coped by moving all of its own output to stderr, which
//! is a rule every future line of code in the process has to keep, forever,
//! with nothing to enforce it. It also makes the process's stdout unusable for
//! its actual purpose: `duet-host-stdio` speaks a *protocol* on stdout, so a
//! Duet host that adopted this trick could not also be a stdio host.
//!
//! The spec (§8.2) flags it too: *"Capturing the VM service URI currently
//! needs an fd-1 redirect… Workable but ugly — look for a cleaner route (a
//! known `--vm-service-port`, or reading it off the engine object) before
//! shipping this in the CLI."*
//!
//! # Two clean routes, and no redirect in either
//!
//! **[`Announcement`] — for `duet dev`, which runs the host as a child
//! process.** The child's stdout is an ordinary [`std::process::Stdio::piped`]
//! pipe. The parent reads it, scans for the announcement, and echoes every
//! line through to its own stdout so the developer still sees their app's
//! output. No fd is redirected, the parent's own stdout is untouched, and the
//! VM service keeps its authentication code. This is the shipping path, and it
//! is not a workaround — it falls out of the architecture the spec already
//! describes, where `duet dev` and the host are separate processes.
//!
//! **[`engine_switches`] — for a host that drives its own reload
//! in-process.** The macOS embedder reads engine switches from the environment
//! (`FLUTTER_ENGINE_SWITCHES`, `FLUTTER_ENGINE_SWITCH_1…N`), so the VM service
//! can be *told* which port to use before it starts, making its URI known
//! without observing any output at all. Measured on this machine: with
//! `vm-service-port=45671` and `disable-service-auth-codes`, the engine
//! announced exactly `http://127.0.0.1:45671/`.
//!
//! The cost of the second route is stated on [`engine_switches`] and is the
//! reason it is not the default.
//!
//! (`write-service-info=<path>` was tried first, since it would have given a
//! known URI *with* the auth code. The string is present in
//! `FlutterMacOS.framework`, but no file appeared: the switch is not wired up
//! in this embedder. Recorded so the next person does not spend the same
//! twenty minutes.)

use std::net::TcpListener;

use crate::error::{DevError, Stage};
use crate::url::VmServiceUrl;

/// Environment variables that make a Flutter engine put its VM service on a
/// known, unauthenticated loopback port.
///
/// Returns `(name, value)` pairs to set **before** the engine starts. The
/// embedder reads `FLUTTER_ENGINE_SWITCHES` for a count and then
/// `FLUTTER_ENGINE_SWITCH_1…N`, prepending `--` to each value itself — which
/// is why the values here carry no leading dashes.
///
/// # Only for debug/JIT builds, and only on loopback
///
/// `disable-service-auth-codes` removes the random path component that
/// otherwise guards the VM service. The VM service binds `127.0.0.1` and
/// exists **only in debug and profile builds** — a release/AOT engine has no
/// VM service to expose, so this cannot weaken a shipped application. Within a
/// debug session it does mean any process on the machine that can reach that
/// loopback port can drive the Dart VM: read memory, evaluate expressions,
/// load code. On a developer's own machine that is the same trust boundary as
/// the debugger already assumes.
///
/// It is nonetheless why [`Announcement`] is the default for `duet dev`: a
/// child process's piped stdout gives the same result with the auth code left
/// on, so nothing has to be given up. Reach for this only when the engine is
/// in *this* process and there is no pipe to read.
pub fn engine_switches(port: u16) -> Vec<(String, String)> {
    vec![
        ("FLUTTER_ENGINE_SWITCHES".to_string(), "2".to_string()),
        (
            "FLUTTER_ENGINE_SWITCH_1".to_string(),
            format!("vm-service-port={port}"),
        ),
        (
            "FLUTTER_ENGINE_SWITCH_2".to_string(),
            "disable-service-auth-codes".to_string(),
        ),
    ]
}

/// Asks the OS for a free loopback port.
///
/// Binds port 0, reads what was assigned, and closes it. There is an
/// unavoidable race between closing and the engine binding — nothing in the
/// sockets API can hand a port from one owner to another — but the window is
/// microseconds and the OS does not immediately reuse a just-released
/// ephemeral port. If it is lost, the engine fails to start its VM service and
/// [`DevError::Timeout`] at [`Stage::LocateVmService`] reports it, rather than
/// anything silently connecting to the wrong process: a different service on
/// that port would fail the WebSocket handshake's accept check.
///
/// # Errors
///
/// [`DevError::Io`] if no loopback port could be bound at all.
pub fn free_port() -> Result<u16, DevError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|source| DevError::Io {
        stage: Stage::LocateVmService,
        doing: "asking the OS for a free port",
        source,
    })?;
    let port = listener
        .local_addr()
        .map_err(|source| DevError::Io {
            stage: Stage::LocateVmService,
            doing: "reading the port the OS assigned",
            source,
        })?
        .port();
    drop(listener);
    Ok(port)
}

/// Recognises the engine's VM service announcement in a stream of output
/// lines.
///
/// Pure: feed it lines from wherever they come from — a child's piped stdout,
/// a log file, a test — and it reports the first URL it recognises. Keeping it
/// separate from the reading is what makes every announcement shape testable
/// without an engine.
#[derive(Debug, Default)]
pub struct Announcement;

impl Announcement {
    /// Returns the VM service URL if `line` is an announcement.
    ///
    /// Matches on the URL rather than on the sentence around it. The engine's
    /// wording has changed before ("Observatory listening on…" became "The
    /// Dart VM service is listening on…") and the line may arrive with a
    /// `flutter: ` prefix depending on how the guest's output is routed; the
    /// `http://127.0.0.1:<port>/` shape is the part that has been stable.
    ///
    /// A line is only accepted if it also mentions listening, so that a
    /// developer's own `print` of some unrelated URL cannot be mistaken for
    /// the announcement.
    pub fn read(&self, line: &str) -> Option<VmServiceUrl> {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("listening on") || lower.contains("vm service")) {
            return None;
        }
        let start = line.find("http://").or_else(|| line.find("https://"))?;
        let candidate = line[start..]
            .split_whitespace()
            .next()
            .unwrap_or(&line[start..]);
        VmServiceUrl::parse(candidate).ok()
    }
}

#[cfg(test)]
#[path = "locate_tests.rs"]
mod tests;
