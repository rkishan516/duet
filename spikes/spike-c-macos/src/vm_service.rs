//! Minimal blocking JSON-RPC 2.0 client over the Dart VM service WebSocket,
//! using `tungstenite`. Just enough to drive hot reload: `getVM` (to find the
//! isolate), `reloadSources` (to apply an incremental kernel diff), and
//! `ext.flutter.reassemble` (to force Flutter to actually rebuild after the
//! reload - flutter_tools does this same follow-up call).

use serde_json::{json, Value};
use std::net::TcpStream;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

pub struct VmServiceClient {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl VmServiceClient {
    pub fn connect(ws_url: &str) -> Self {
        let (socket, _response) =
            connect(ws_url).unwrap_or_else(|e| panic!("failed to connect to VM service ws {ws_url}: {e}"));
        Self { socket, next_id: 1 }
    }

    /// Sends a JSON-RPC 2.0 request and blocks until the matching-id reply
    /// arrives, discarding any interleaved event/notification frames the VM
    /// service also pushes down the same socket (Isolate pause/resume
    /// events, GC events, extension events, etc. - none of which carry a
    /// matching "id").
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(req.to_string().into()))
            .unwrap_or_else(|e| panic!("ws send failed for {method}: {e}"));
        loop {
            let msg = self
                .socket
                .read()
                .unwrap_or_else(|e| panic!("ws read failed waiting for {method} reply: {e}"));
            if let Message::Text(txt) = msg {
                let v: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
                if v.get("id").and_then(Value::as_u64) == Some(id) {
                    return v;
                }
                // else: unrelated notification on the shared stream, keep waiting.
            }
        }
    }

    pub fn get_vm(&mut self) -> Value {
        self.call("getVM", json!({}))
    }

    pub fn reload_sources(&mut self, isolate_id: &str, root_lib_uri: &str) -> Value {
        // NOTE: deliberately NOT passing "force": true. Per the VM service docs,
        // `force` reloads ALL libraries, not just changed ones - but our delta
        // dill only contains main.dart's recompiled library, not a full copy of
        // e.g. framework.dart. flutter_tools' own reloadSources call (run_hot.dart)
        // never passes `force` either. Passing it here reproduced a VM-fatal
        // crash: "Unable to use class Library:'package:flutter/src/widgets/
        // framework.dart' Class: StatelessWidget which is not loaded yet" - see
        // FINDINGS.md.
        self.call(
            "reloadSources",
            json!({
                "isolateId": isolate_id,
                "rootLibUri": root_lib_uri,
            }),
        )
    }

    pub fn reassemble(&mut self, isolate_id: &str) -> Value {
        self.call("ext.flutter.reassemble", json!({"isolateId": isolate_id}))
    }
}

/// `http://127.0.0.1:PORT/AUTHCODE/` -> `ws://127.0.0.1:PORT/AUTHCODE/ws`
pub fn http_to_ws_url(http_base_url: &str) -> String {
    let ws_base = http_base_url.replacen("http://", "ws://", 1);
    format!("{ws_base}ws")
}
