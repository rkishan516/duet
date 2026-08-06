//! The Dart VM service client: the three RPCs a hot reload needs.
//!
//! `getVM` to find the isolate, `reloadSources` to apply an incremental kernel
//! diff, `ext.flutter.reassemble` to make Flutter rebuild the widget tree with
//! the new code. Spike C established that exact sequence works against a
//! Rust-hosted engine (`spikes/spike-c-macos/FINDINGS.md`); this is that
//! sequence with deadlines, typed errors and no `unwrap`.
//!
//! # `force: true` is not a parameter you can pass
//!
//! Spike C's most expensive finding was that `"force": true` on
//! `reloadSources` **aborts the Dart VM**, inside its own C++ runtime, with
//! `Unable to use class Library:'package:flutter/src/widgets/framework.dart'
//! Class: StatelessWidget which is not loaded yet`. It is not a recoverable
//! RPC error; the process dies. The cause: `force` asks the VM to reload every
//! library, but an incremental diff only *contains* the changed one, so the VM
//! is told to re-finalize declarations it was not given. `flutter_tools`'
//! own call (`run_hot.dart`) never passes it.
//!
//! The defence here is structural rather than a comment. `ReloadSources` has
//! no `force` field, [`VmServiceClient::reload_sources`] builds its params
//! from that struct alone, and a test asserts the encoded request has no such
//! key. Reintroducing the crash takes editing a struct definition and deleting
//! a test, not forgetting a default.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{DevError, Stage};
use crate::jsonrpc::{self, Correlation};
use crate::url::VmServiceUrl;
use crate::ws::WebSocket;

/// A Dart isolate's VM-service id, such as `isolates/7546815380299399`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolateId(pub String);

/// What `reloadSources` reported.
///
/// The counts are what distinguish a real incremental reload from a no-op or a
/// full reload: Spike C measured `savedLibraryCount: 752` against
/// `receivedLibraryCount: 2` on a one-line edit, and a driver that stopped at
/// `success: true` could not tell those apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadReport {
    /// Whether the VM accepted the diff.
    pub success: bool,
    /// Libraries carried in the diff.
    pub received_libraries: Option<i64>,
    /// Libraries kept from the previous generation — the number that proves
    /// this was incremental.
    pub saved_libraries: Option<i64>,
    /// The VM's own explanation when `success` is false.
    pub notices: Vec<String>,
}

/// The one shape a `reloadSources` request may take here. See this module's
/// docs: the absence of a `force` field is the point.
#[derive(Debug, Clone)]
struct ReloadSources<'a> {
    isolate: &'a IsolateId,
    /// A `file://` URI pointing at the incremental dill `frontend_server`
    /// just wrote.
    root_lib_uri: &'a str,
}

impl ReloadSources<'_> {
    fn params(&self) -> Value {
        json!({
            "isolateId": self.isolate.0,
            "rootLibUri": self.root_lib_uri,
        })
    }
}

/// A connected Dart VM service.
pub struct VmServiceClient {
    socket: WebSocket,
    next_id: u64,
}

impl VmServiceClient {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// [`DevError::Io`], [`DevError::Timeout`] or [`DevError::VmService`] at
    /// [`Stage::Connect`] — see `WebSocket::connect`.
    pub fn connect(url: &VmServiceUrl, timeout: Duration) -> Result<Self, DevError> {
        Ok(VmServiceClient {
            socket: WebSocket::connect(url, timeout)?,
            next_id: 1,
        })
    }

    /// Sends a request and waits for its reply, discarding events.
    ///
    /// # Errors
    ///
    /// [`DevError::Timeout`] at `stage` if no reply arrived in `timeout`,
    /// [`DevError::VmService`] if the server reported an error or sent
    /// something undecodable.
    fn call(
        &mut self,
        stage: Stage,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, DevError> {
        let id = self.next_id;
        // Wrapping rather than overflowing: a `duet dev` session left running
        // for a week is a normal thing, and an id that has wrapped is still
        // unique against anything still in flight.
        self.next_id = self.next_id.wrapping_add(1).max(1);

        self.socket
            .send_text(&jsonrpc::request(id, method, params))?;

        // One deadline for the whole exchange, not per frame: an event storm
        // must not be able to extend the wait for our own reply.
        let deadline = Instant::now() + timeout;
        loop {
            let text = self.socket.next_text(stage, deadline)?;
            match jsonrpc::correlate(&text, id) {
                Correlation::Reply(result) => return Ok(*result),
                Correlation::Failed(detail) => {
                    return Err(DevError::vm(stage, format!("{method} {detail}")));
                }
                Correlation::Undecodable(text) => {
                    return Err(DevError::vm(
                        stage,
                        format!("sent something that is not JSON-RPC: {text}"),
                    ));
                }
                Correlation::Other => {}
            }
        }
    }

    /// Finds the isolate to reload.
    ///
    /// Takes the **first** isolate `getVM` reports, which is the main isolate
    /// in every configuration this project boots: one `FlutterEngine`, one
    /// `runWithEntrypoint`, no `Isolate.spawn` in any guest. If a guest ever
    /// spawns one, this becomes wrong quietly — so the count is reported in
    /// the error when there are none, and callers log which id was chosen.
    ///
    /// # Errors
    ///
    /// [`DevError::VmService`] if the VM reported no isolates or a reply this
    /// client cannot read, [`DevError::Timeout`] at [`Stage::FindIsolate`].
    pub fn main_isolate(&mut self, timeout: Duration) -> Result<IsolateId, DevError> {
        let vm = self.call(Stage::FindIsolate, "getVM", json!({}), timeout)?;
        let isolates = vm.get("isolates").and_then(Value::as_array);
        let first = isolates
            .and_then(|list| list.first())
            .and_then(|iso| iso.get("id"))
            .and_then(Value::as_str);
        match first {
            Some(id) => Ok(IsolateId(id.to_string())),
            None => Err(DevError::vm(
                Stage::FindIsolate,
                format!(
                    "getVM reported {} isolate(s); expected at least one",
                    isolates.map_or(0, Vec::len)
                ),
            )),
        }
    }

    /// Applies an incremental kernel diff.
    ///
    /// `root_lib_uri` must be a `file://` URI for the `.incremental.dill`
    /// `frontend_server` wrote. **Never sends `force`** — see this module's
    /// docs for the VM-fatal crash that causes.
    ///
    /// A `success: false` report is returned as `Ok`, not an error: a reload
    /// the VM declines (a change it cannot apply incrementally) is a normal
    /// event in a dev loop, and the caller decides what to do about it.
    ///
    /// # Errors
    ///
    /// [`DevError::VmService`] if the RPC itself failed or the report could
    /// not be read, [`DevError::Timeout`] at [`Stage::ReloadSources`].
    pub fn reload_sources(
        &mut self,
        isolate: &IsolateId,
        root_lib_uri: &str,
        timeout: Duration,
    ) -> Result<ReloadReport, DevError> {
        let request = ReloadSources {
            isolate,
            root_lib_uri,
        };
        let result = self.call(
            Stage::ReloadSources,
            "reloadSources",
            request.params(),
            timeout,
        )?;
        read_reload_report(&result).ok_or_else(|| {
            DevError::vm(Stage::ReloadSources, format!("unreadable report: {result}"))
        })
    }

    /// Makes Flutter rebuild the widget tree with the reloaded code.
    ///
    /// Sent after every successful `reloadSources`, matching `flutter_tools`.
    /// Spike C could not isolate whether it is strictly required — its fixture
    /// had a `Ticker` requesting frames anyway — so this keeps parity rather
    /// than betting on it.
    ///
    /// # Errors
    ///
    /// [`DevError::VmService`] or [`DevError::Timeout`] at
    /// [`Stage::Reassemble`].
    pub fn reassemble(&mut self, isolate: &IsolateId, timeout: Duration) -> Result<(), DevError> {
        self.call(
            Stage::Reassemble,
            "ext.flutter.reassemble",
            json!({ "isolateId": isolate.0 }),
            timeout,
        )?;
        Ok(())
    }
}

/// Reads a `ReloadReport` out of `reloadSources`' result.
///
/// A free function so the shape can be tested against real captured payloads
/// without a socket. Returns `None` only when `success` is missing or not a
/// boolean — everything else is optional, because the VM's `details` object
/// has gained fields across SDK versions and a driver that required all of
/// them would break on an SDK bump for no reason.
fn read_reload_report(result: &Value) -> Option<ReloadReport> {
    let success = result.get("success")?.as_bool()?;
    let details = result.get("details");
    let count = |key: &str| details.and_then(|d| d.get(key)).and_then(Value::as_i64);
    let notices = details
        .and_then(|d| d.get("notices"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|n| n.get("message").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(ReloadReport {
        success,
        received_libraries: count("receivedLibraryCount"),
        saved_libraries: count("savedLibraryCount"),
        notices,
    })
}

#[cfg(test)]
#[path = "vm_service_tests.rs"]
mod tests;
