//! The HTML and JavaScript a webview guest boots with.

/// A minimal guest client: `__duet.get/set/subscribe`, correlated by request id.
///
/// Phase 4's codegen will generate a typed client over this same protocol; this
/// is the hand-written floor that proves the transport works.
pub const BOOTSTRAP_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Duet webview surface</title></head>
<body style="font-family: system-ui; padding: 1rem">
<h1>Duet webview surface</h1>
<pre id="log">booting…</pre>
<script>
(function () {
  const pending = new Map();
  let nextId = 1;

  function send(msg) {
    // wry delivers this to the Rust ipc_handler as a string.
    window.ipc.postMessage(JSON.stringify(msg));
  }

  function call(kind, extra) {
    const id = String(nextId++);
    return new Promise((resolve) => {
      pending.set(id, resolve);
      send(Object.assign({ kind, id }, extra));
    });
  }

  window.__duet = {
    get: (path) => call("get", { path }),
    set: (path, value) => call("set", { path, value }),
    subscribe: (path) => call("subscribe", { path }),
    pushes: [],
    log: [],

    onResponse(response) {
      const resolve = pending.get(response.id);
      if (resolve) {
        pending.delete(response.id);
        resolve(response);
      }
      window.__duet.log.push(response);
      document.getElementById("log").textContent =
        JSON.stringify(window.__duet.log, null, 1);
    },

    onPush(push) {
      window.__duet.pushes.push(push);
    },
  };

  document.getElementById("log").textContent = "ready";
})();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bootstrap_defines_the_hooks_the_host_calls() {
        // The host emits `window.__duet.onResponse(...)` and
        // `window.__duet.onPush(...)`. If the bootstrap stops defining
        // either, every reply is silently dropped and the guest hangs
        // forever — there is no error, just silence.
        assert!(
            BOOTSTRAP_HTML.contains("onResponse"),
            "bootstrap must define onResponse"
        );
        assert!(
            BOOTSTRAP_HTML.contains("onPush"),
            "bootstrap must define onPush"
        );
        assert!(
            BOOTSTRAP_HTML.contains("window.ipc.postMessage"),
            "bootstrap must send on wry's IPC channel"
        );
    }

    #[test]
    fn the_emitted_scripts_target_the_hooks_the_bootstrap_defines() {
        // Pins the two halves against each other. Each side is individually
        // plausible; only together do they form a working channel, so a
        // rename on either side alone must fail here.
        let response = crate::response_script(r#"{"kind":"done","id":"1"}"#);
        assert!(response.contains("__duet.onResponse"), "got {response}");
        assert!(
            BOOTSTRAP_HTML.contains("onResponse("),
            "bootstrap must define what the host calls"
        );

        let note = duet_core::Notification {
            subscriber: duet_core::SubscriberId(1),
            subscription: duet_core::SubscriptionId(1),
            patch: duet_core::Patch {
                path: duet_core::Path::parse("a").expect("path"),
                value: duet_core::Value::Int(1),
            },
        };
        let push = crate::push_script(&duet_protocol::Push::Notification(note));
        assert!(push.contains("__duet.onPush"), "got {push}");
        assert!(
            BOOTSTRAP_HTML.contains("onPush("),
            "bootstrap must define what the host calls"
        );
    }
}
