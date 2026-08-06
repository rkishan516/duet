//! A fake `frontend_server`, for tests only.
//!
//! The real one needs a Flutter SDK, which CI does not have (`duet.yml` runs
//! on `ubuntu-latest` with no Flutter toolchain). It also cannot be made to
//! die on demand, hang on demand, or emit a protocol shape from a future SDK —
//! and those are precisely the paths [`crate::frontend_server`] exists to
//! survive.
//!
//! So these tests drive a real child process that speaks the same line
//! protocol, scripted in POSIX `sh`. It is a real `Command`, real pipes, real
//! EOF and real exit statuses; only the compiler is fake.
//!
//! `#[cfg(unix)]` throughout: this workspace's CI is Linux, its backend is
//! macOS, and no part of the project claims Windows support. Faking a child
//! process portably would mean a second implementation of the fake, which is
//! more code to be wrong than the thing it tests.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::frontend_server::CompilerConfig;
use crate::sdk::FlutterSdk;

/// A scratch directory that removes itself.
pub(crate) struct Scratch {
    pub(crate) path: PathBuf,
}

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("duet-dev-compiler-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory should be creatable");
        Scratch { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The `sh` fragment that answers one `compile`/`recompile`.
///
/// `$KEY` is the boundary the fake chooses for itself — deliberately *not* the
/// one the request carried, because the real server picks its own and a parser
/// that assumed otherwise would work here and fail in production.
const REPLY_FUNCTION: &str = r#"
reply() {
  KEY="server-chosen-boundary"
  echo "result $KEY"
  emit_diagnostics
  echo "$KEY$TERMINATOR_TAIL"
}
"#;

/// Builds a fake compiler script and the config that runs it.
///
/// `emit_diagnostics` is `sh` that writes zero or more diagnostic lines;
/// `terminator_tail` is what follows the boundary key on the terminator line
/// (empty for the bare form this SDK actually produces).
pub(crate) fn config_for(scratch: &Scratch, body: &str) -> CompilerConfig {
    let script = scratch.path.join("fake_frontend_server.sh");
    std::fs::write(&script, body).expect("the fake compiler should be writable");
    CompilerConfig {
        sdk: FlutterSdk {
            // `Command::new(dartaotruntime).arg(frontend_server)` becomes
            // `sh <script> --sdk-root … --incremental …`, so the flags land as
            // positional arguments the script ignores.
            dartaotruntime: PathBuf::from("/bin/sh"),
            frontend_server: script,
            patched_sdk: scratch.path.join("patched_sdk"),
        },
        package_config: scratch.path.join("package_config.json"),
        output_dill: scratch.path.join("out").join("app.dill"),
    }
}

/// A compiler that answers every request cleanly, with the bare-boundary
/// terminator this repository's SDK actually produces.
pub(crate) fn clean_script() -> String {
    script("emit_diagnostics() { :; }", "TERMINATOR_TAIL=''", "")
}

/// A compiler that reports two errors, with the documented terminator that
/// carries a dill path and an error count.
pub(crate) fn failing_script(dill: &Path) -> String {
    script(
        "emit_diagnostics() {\n  echo \"lib/main.dart:4:12: Error: Expected ';' after this.\"\n  echo \"lib/main.dart:9:1: Error: Undefined name 'oops'.\"\n}",
        &format!("TERMINATOR_TAIL=' {} 2'", dill.display()),
        "",
    )
}

/// A compiler that succeeds and reports its dill path and a zero error count.
pub(crate) fn clean_with_dill_script(dill: &Path) -> String {
    script(
        "emit_diagnostics() { :; }",
        &format!("TERMINATOR_TAIL=' {} 0'", dill.display()),
        "",
    )
}

/// Assembles a script from its three variable parts.
///
/// The loop consumes the two extra lines a `recompile` sends (the invalidated
/// URIs and the echoed boundary), which is what keeps the fake in step with a
/// three-line request.
fn script(diagnostics: &str, terminator: &str, extra: &str) -> String {
    format!(
        r#"#!/bin/sh
{diagnostics}
{terminator}
{REPLY_FUNCTION}
{extra}
while IFS= read -r line; do
  case "$line" in
    compile*) reply ;;
    recompile*)
      key=${{line#recompile }}
      while IFS= read -r inner; do
        [ "$inner" = "$key" ] && break
      done
      reply
      ;;
    accept) echo "$ACCEPT_ECHO" ;;
    reject) ;;
    quit) exit 0 ;;
  esac
done
"#
    )
}

/// A compiler that writes to stderr and exits before saying anything — what a
/// bad `--sdk-root` or a snapshot from another SDK looks like.
pub(crate) fn dies_immediately_script() -> String {
    r#"#!/bin/sh
echo "Unhandled exception:" >&2
echo "FileSystemException: Cannot open file, path = '/nope/flutter_patched_sdk/lib/libraries.json'" >&2
exit 253
"#
    .to_string()
}

/// A compiler that answers the first request and then dies — the harder case,
/// because the driver has already seen it working.
pub(crate) fn dies_after_one_script() -> String {
    r#"#!/bin/sh
IFS= read -r line
echo "result k"
echo "k"
IFS= read -r line
echo "the compiler crashed while recompiling" >&2
exit 70
"#
    .to_string()
}

/// A compiler that accepts input and never answers.
pub(crate) fn silent_script() -> String {
    r#"#!/bin/sh
while IFS= read -r line; do
  :
done
sleep 60
"#
    .to_string()
}

/// A compiler that talks, but never emits a `result` line.
pub(crate) fn garbage_script() -> String {
    r#"#!/bin/sh
IFS= read -r line
echo "Warning: something went sideways"
echo "not a result line"
echo "still not"
sleep 60
"#
    .to_string()
}

/// A compiler whose `accept` echoes a delayed confirmation, which arrives
/// interleaved with the *next* command's output — the real quirk Spike C
/// recorded.
pub(crate) fn noisy_accept_script() -> String {
    script(
        "emit_diagnostics() { :; }",
        "TERMINATOR_TAIL=''",
        "ACCEPT_ECHO='previous-boundary /tmp/out.dill 3'",
    )
}
