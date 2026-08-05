//! The HTML and JavaScript a webview guest boots with.

/// A minimal guest client: `__duet.get/set/subscribe`, correlated by request id.
///
/// Phase 4's codegen will generate a typed client over this same protocol; this
/// is the hand-written floor that proves the transport works.
///
/// # Two encoding rules a JavaScript guest must not get wrong
///
/// This client does **not** encode `duet_core::Value` for you: `set` takes an
/// already-tagged object (`{t:"i", v:"42"}`). Two of those tags have payloads
/// JavaScript cannot spell naively, so both are called out in the script's own
/// comments and given helpers below.
///
/// **Ids are canonical decimal strings.** `String(nextId++)` is already
/// canonical; hand-writing `"007"` is not, and `duet-protocol` rejects it. It
/// must, because the host echoes ids back in canonical form — a guest that
/// sent `"007"` would be answered `"7"`, never match its own pending entry,
/// and hang with no error at all.
///
/// **Negative zero needs a sentinel.** `JSON.stringify(-0)` is `"0"`, so a JS
/// guest cannot express negative zero as a JSON number under any encoding
/// short of a hand-rolled serializer. `__duet.float()` maps it to the string
/// `"-0"`, joining `"NaN"`, `"Infinity"` and `"-Infinity"` — the four `f64`
/// values with no portable JSON-number spelling.
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
    // A CANONICAL decimal string: no leading "+", no leading zeros. The host
    // echoes the canonical form back, so an id spelled "007" would be answered
    // "7", never match this map, and hang forever with no error. String(n) on
    // a non-negative integer is always canonical; do not hand-write ids.
    const id = String(nextId++);
    return new Promise((resolve) => {
      pending.set(id, resolve);
      send(Object.assign({ kind, id }, extra));
    });
  }

  // Exactly four f64 values have no portable JSON-number spelling, so all four
  // travel as string sentinels. NaN and the infinities have no JSON literal at
  // all; -0 has one, but JSON.stringify(-0) is "0", so a JS guest cannot emit
  // it. Object.is(n, -0) is the only reliable test here: n === -0 is true for
  // +0 too.
  function encodeFloat(n) {
    if (Number.isNaN(n)) { return "NaN"; }
    if (n === Infinity) { return "Infinity"; }
    if (n === -Infinity) { return "-Infinity"; }
    if (Object.is(n, -0)) { return "-0"; }
    return n;
  }

  // The inverse. The host may send a float as a JSON number or as any of the
  // four sentinels, so a guest reading one must go through this.
  function decodeFloat(v) {
    if (typeof v === "number") { return v; }
    if (v === "NaN") { return NaN; }
    if (v === "Infinity") { return Infinity; }
    if (v === "-Infinity") { return -Infinity; }
    if (v === "-0") { return -0; }
    throw new Error("unrecognised float sentinel: " + String(v));
  }

  window.__duet = {
    get: (path) => call("get", { path }),
    set: (path, value) => call("set", { path, value }),
    subscribe: (path) => call("subscribe", { path }),

    // Builds a tagged float value for `set`. Use this rather than writing
    // {t:"f", v:n} by hand: it is the only path that keeps -0's sign.
    float: (n) => ({ t: "f", v: encodeFloat(n) }),

    // Reads a tagged float value back out of a response or push.
    toFloat: (value) => decodeFloat(value.v),

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
    fn the_bootstrap_handles_every_float_sentinel() {
        // The four f64 values with no portable JSON-number spelling. `-0` is
        // the one a JS guest cannot express as a number at all
        // (JSON.stringify(-0) is "0"), so its absence here would silently drop
        // the sign on every float this guest sends — no error, just a wrong
        // value. Pinned as strings because these tests cannot run JavaScript.
        for sentinel in ["\"NaN\"", "\"Infinity\"", "\"-Infinity\"", "\"-0\""] {
            assert!(
                BOOTSTRAP_HTML.contains(sentinel),
                "bootstrap must handle the {sentinel} float sentinel"
            );
        }
        // Object.is is the only correct negative-zero test in JavaScript:
        // `n === -0` is also true for +0, so a guest using === would encode
        // every zero as "-0".
        assert!(
            BOOTSTRAP_HTML.contains("Object.is(n, -0)"),
            "bootstrap must detect -0 with Object.is, not ==="
        );
        assert!(
            BOOTSTRAP_HTML.contains("float:") && BOOTSTRAP_HTML.contains("toFloat:"),
            "bootstrap must expose both halves of the float codec"
        );
    }

    #[test]
    fn the_bootstrap_sends_canonical_ids() {
        // `duet-protocol` rejects a non-canonical id, and the host echoes the
        // canonical form back — so a guest that sent "007" would be answered
        // "7", never match `pending`, and hang. `String(nextId++)` on a
        // non-negative integer is canonical by construction; anything that
        // pads or prefixes it would reintroduce the hang.
        assert!(
            BOOTSTRAP_HTML.contains("String(nextId++)"),
            "bootstrap must derive ids from a plain integer counter"
        );
        assert!(
            BOOTSTRAP_HTML.contains("CANONICAL"),
            "bootstrap must document why the id spelling matters"
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
