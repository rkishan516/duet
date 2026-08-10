//! The hot-reload driver behind `duet dev`.
//!
//! A Flutter engine in JIT mode can be hot-reloaded over the Dart VM service,
//! driven by a `frontend_server` this crate manages itself, with no
//! `flutter_tools` process involved. Spike C proved that against a
//! Rust-hosted engine and measured it: **median 123.3 ms edit-to-frame, n =
//! 10, well inside the 500 ms parity bar**, with Dart heap state surviving
//! every reload (`spikes/spike-c-macos/FINDINGS.md`). This crate is that
//! recipe, productionised.
//!
//! # The cycle
//!
//! ```text
//!   a .dart file changes
//!            v
//!   Watcher (debounced)                      watch.rs
//!            v
//!   frontend_server: recompile -> accept     frontend_server.rs
//!            v
//!   VM service: reloadSources                vm_service.rs
//!            v
//!   VM service: ext.flutter.reassemble       vm_service.rs
//! ```
//!
//! [`ReloadDriver`] is the whole of it; everything else is a piece it uses.
//!
//! # Four rules this crate is built around
//!
//! **Never `force: true` on `reloadSources`.** It aborts the Dart VM inside
//! its own C++ runtime — not a recoverable error, the process dies. Spike C
//! lost most of a day to it. The defence is structural, not a comment: see
//! [`vm_service`].
//!
//! **No `unwrap`, `expect` or `panic!` outside tests.** A dev tool that
//! panics on a syntax error is worse than useless, because the developer now
//! has to restart the session as well as fix the typo. A compile error is not
//! even an error here — it is [`Reload::CompileFailed`], an `Ok` value.
//!
//! **Nothing waits without a deadline, and every failure names a stage.**
//! A reload involves a child process's pipe, a TCP connect and two JSON-RPC
//! round trips, any of which can hang forever. [`Stage`] is on every
//! [`DevError`], so "it hung" is always "it hung at `reload-sources`".
//!
//! **No fd-1 redirect.** Spike C recovered the VM service URI by `dup2`-ing
//! its own stdout, which swallows every other thing the process prints.
//! [`locate`] retires that two ways, neither of which touches a file
//! descriptor.
//!
//! # Scope
//!
//! The Dart half of the spec's §8.2 `duet dev` diagram. The web half (a Vite
//! dev server for HMR) and Rust-side restart-on-change are not here — the
//! first is a process this crate does not need to understand to supervise, and
//! the second is a plain rebuild-and-restart with no protocol to it.
//!
//! This crate is platform-independent: it drives a child process, a TCP
//! socket, and the filesystem. Only [`locate::engine_switches`] is documented
//! against a specific embedder, and it is inert on any other.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod base64;
mod compiler_protocol;
mod frame;
mod jsonrpc;
mod sha1;
mod ws;

#[cfg(test)]
mod test_compiler;
#[cfg(test)]
mod test_server;

pub mod error;
pub mod frontend_server;
pub mod locate;
pub mod packages;
pub mod reload;
pub mod sdk;
pub mod url;
pub mod vm_service;
pub mod watch;

pub use error::{DevError, Stage};
pub use frontend_server::{CompileOutcome, CompilerConfig, FrontendServer, file_uri};
pub use locate::{Announcement, engine_switches, free_port};
pub use packages::PackageConfig;
pub use reload::{DriverConfig, Reload, ReloadDriver, Started, Timeouts, Timings};
pub use sdk::{DART_AOT_RUNTIME, FlutterSdk, package_config};
pub use url::{UrlError, VmServiceUrl};
pub use vm_service::{IsolateId, ReloadReport, VmServiceClient};
pub use watch::{WatchConfig, Watcher};
