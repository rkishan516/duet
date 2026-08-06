//! The HTML and JavaScript a webview guest boots with.

/// A minimal guest client: `__duet.get/set/subscribe`, correlated by request id.
///
/// Phase 4's codegen will generate a typed client over this same protocol; this
/// is the hand-written floor that proves the transport works.
///
/// # This is not the client a real app uses
///
/// There are two JavaScript implementations of this wire format in the
/// repository, and the split is deliberate:
///
/// - **This constant** is the *default page* a `WebviewSurface` boots with when
///   the embedder supplies no HTML of its own. It exists so the transport can
///   be exercised end to end with nothing installed, and so the macOS examples
///   have a guest to drive. It deliberately does **not** encode
///   `duet_core::Value` for you — `set` takes an already-tagged object.
/// - **`packages/duet-js`** (published as `duet-protocol` on npm) is the full
///   typed client, with the value codec, the envelope codec, path parsing, a
///   `bigint` integer domain and a bounded-depth JSON decoder. That is what a
///   real webview app installs.
///
/// Building this constant *from* that package was considered and rejected. It
/// would make every `cargo build` depend on a committed bundler artifact —
/// meaning a Rust-only contributor could not change this page, and a stale
/// artifact would be caught only by a CI job that needs Node. It would also
/// replace a readable 130-line script, debuggable as-is in the webview
/// inspector, with a minified blob, and would force the examples onto the npm
/// package's `bigint`/`Map` API for no gain at this layer.
///
/// The cost of keeping them separate is **drift**, and the tests below can only
/// assert that substrings are present, because a Rust test cannot execute
/// JavaScript. `packages/duet-js/test/bootstrap-parity.test.ts` closes that gap:
/// it reads the script out of *this file*, runs it in a `node:vm` sandbox, and
/// checks it against `corpus/wire-corpus.json` — the same golden corpus every
/// other implementation is checked against — for the subset this script can
/// reach: the float sentinels, the code-point key comparator, canonical request
/// ids, and the response hook. The `the_parity_suite_that_executes_this_script_exists`
/// test below keeps that file from being deleted without this side noticing.
///
/// # Three encoding rules a JavaScript guest must not get wrong
///
/// This client does **not** encode `duet_core::Value` for you: `set` takes an
/// already-tagged object (`{t:"i", v:"42"}`). Three of those constructs have
/// spellings JavaScript gets wrong naively, so each is called out in the
/// script's own comments and given a helper below.
///
/// **Ids are canonical decimal strings in `0..=i64::MAX`.**
/// `String(nextId++)` is already canonical; hand-writing `"007"` is not, and
/// `duet-protocol` rejects it. It must, because the host echoes ids back in
/// canonical form — a guest that sent `"007"` would be answered `"7"`, never
/// match its own pending entry, and hang with no error at all. The upper bound
/// exists because Dart's native `int` is 64-bit signed (see
/// `duet_codec::MAX_WIRE_ID`); a JS counter reaches `Number.MAX_SAFE_INTEGER`
/// long before it, so it constrains nothing in practice here.
///
/// **Negative zero needs a sentinel.** `JSON.stringify(-0)` is `"0"`, so a JS
/// guest cannot express negative zero as a JSON number under any encoding
/// short of a hand-rolled serializer. `__duet.float()` maps it to the string
/// `"-0"`, joining `"NaN"`, `"Infinity"` and `"-Infinity"` — the four `f64`
/// values with no portable JSON-number spelling.
///
/// **Map keys travel in code-point order.** JavaScript's default
/// `Array.prototype.sort` compares UTF-16 **code units**, which disagrees with
/// code-point order above the BMP: `U+1F600` encodes as the surrogate pair
/// `D83D DE00`, and `0xD83D < 0xE000`, so a default sort puts it before
/// `U+E000` where Rust's `BTreeMap<String>` puts it after. `__duet.map()`
/// builds a `{t:"m"}` value with an explicit code-point comparator.
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

  // Map keys travel in CODE-POINT order, which is also UTF-8 byte order and so
  // matches what Rust's BTreeMap<String> emits.
  //
  // DO NOT "simplify" this to keys.sort(), a < b, or a.localeCompare(b). The
  // default sort compares UTF-16 CODE UNITS: "\u{1F600}" is encoded as the
  // surrogate pair D83D DE00, and 0xD83D is LESS than 0xE000, so a default sort
  // puts it before "" while Rust puts it after. Every key below U+D800
  // compares the same under both rules, so ASCII test data cannot catch this.
  //
  // Array.from splits a string into CODE POINTS (surrogate pairs stay whole);
  // indexing a string yields code units and reintroduces the bug.
  function compareCodePoints(a, b) {
    const ac = Array.from(a);
    const bc = Array.from(b);
    const shared = Math.min(ac.length, bc.length);
    for (let i = 0; i < shared; i++) {
      const x = ac[i].codePointAt(0);
      const y = bc[i].codePointAt(0);
      if (x !== y) { return x < y ? -1 : 1; }
    }
    // A prefix sorts before the longer string it prefixes.
    return ac.length - bc.length;
  }

  // Builds a tagged map value for `set`, keys in canonical order. Takes an
  // object of ALREADY-TAGGED values: __duet.map({a: __duet.float(1.5)}).
  //
  // CAVEAT no JS guest can work around: JSON.stringify emits an object's
  // INTEGER-LIKE keys ("0", "1", "42") first and in ascending numeric order,
  // whatever order they were inserted in. That is ECMAScript's own property
  // ordering and no sort can override it, so a map whose keys look like array
  // indices cannot be emitted in canonical order from a plain JS object. Such a
  // value still DECODES correctly — the host sorts into a BTreeMap on arrival,
  // and decoding is order-insensitive — but its bytes differ, so a value with
  // integer-like keys must not be used as a golden-corpus fixture captured from
  // this guest.
  function map(entries) {
    const sorted = {};
    for (const key of Object.keys(entries).sort(compareCodePoints)) {
      sorted[key] = entries[key];
    }
    return { t: "m", v: sorted };
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

    // Builds a tagged map value for `set`. Use this rather than writing
    // {t:"m", v:{...}} by hand: it is the only path that orders keys the way
    // Rust does.
    map: map,

    // Exposed so a guest building a nested structure by hand can apply the
    // same order without reimplementing it (and reimplementing it wrongly).
    compareKeys: compareCodePoints,

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
    fn the_bootstrap_orders_map_keys_by_code_point() {
        // JavaScript's default sort compares UTF-16 CODE UNITS, so it puts
        // U+1F600 (surrogate pair D83D DE00, lead unit 0xD83D) BEFORE U+E000,
        // while Rust's BTreeMap<String> — the canonical order — puts it after.
        // A guest that hand-writes {t:"m", v:{...}} inherits whatever order it
        // happened to insert in, so the bootstrap must both provide a correct
        // helper and say why the obvious alternatives are wrong. Pinned as
        // strings because these tests cannot run JavaScript.
        assert!(
            BOOTSTRAP_HTML.contains("map:"),
            "bootstrap must expose a map builder, not leave key order to the guest"
        );
        assert!(
            BOOTSTRAP_HTML.contains("Array.from(a)"),
            "the comparator must split into CODE POINTS; indexing a string \
             yields code units and reintroduces the bug"
        );
        assert!(
            BOOTSTRAP_HTML.contains("codePointAt(0)"),
            "the comparator must compare code points"
        );
        assert!(
            BOOTSTRAP_HTML.contains("localeCompare"),
            "the comparator must warn against the wrong alternatives by name, \
             or a future maintainer will 'simplify' it back to one of them"
        );
        // The caveat a JS guest genuinely cannot code around: JSON.stringify
        // orders integer-like keys numerically first, whatever a sort did.
        assert!(
            BOOTSTRAP_HTML.contains("INTEGER-LIKE"),
            "bootstrap must document that integer-like keys cannot be emitted \
             in canonical order from a plain JS object"
        );
    }

    #[test]
    fn the_bootstrap_and_the_rust_encoder_agree_on_the_canonical_key_order() {
        // Pins the two halves against each other: the JS comparator's contract
        // is "produce what Rust produces", so this test states the Rust side of
        // that claim right next to the JS side, using the same three keys the
        // duet-codec and Dart tests use. The keys straddle the surrogate range
        // deliberately — below U+D800 every implementation already agrees, so
        // ASCII keys would prove nothing.
        let encoded = serde_json::to_string(&duet_codec::encode_value(&duet_core::Value::map([
            ("\u{1F600}", duet_core::Value::Null),
            ("\u{E000}", duet_core::Value::Null),
            ("\u{FFFD}", duet_core::Value::Null),
        ])))
        .expect("serializes");
        assert_eq!(
            encoded,
            "{\"t\":\"m\",\"v\":{\
             \"\u{E000}\":{\"t\":\"n\"},\
             \"\u{FFFD}\":{\"t\":\"n\"},\
             \"\u{1F600}\":{\"t\":\"n\"}}}",
            "the order __duet.map must reproduce: E000, FFFD, 1F600"
        );
    }

    #[test]
    fn the_parity_suite_that_executes_this_script_exists() {
        // Every other test in this module can only grep a string constant,
        // because a Rust test cannot run JavaScript. The one suite that
        // actually *executes* this script — against the same golden corpus the
        // Rust and Dart implementations are checked against — lives in the npm
        // package, so nothing on this side would notice it being deleted. This
        // test is that notice.
        //
        // Asserted on content, not merely existence: an empty file at the right
        // path would pass a `Path::exists` check while proving nothing.
        let parity = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/duet-js/test/bootstrap-parity.test.ts");
        let source = std::fs::read_to_string(&parity).unwrap_or_else(|e| {
            panic!(
                "cannot read {}: {e}\n\nBOOTSTRAP_HTML is one of two JavaScript \
                 implementations of this wire format. That suite is what keeps them \
                 from drifting apart; do not remove it without replacing the check.",
                parity.display()
            )
        });
        assert!(
            source.contains("bootstrap.rs"),
            "the parity suite must load the script out of this file, not from a copy"
        );
        assert!(
            source.contains("wire-corpus.json"),
            "the parity suite must check the bootstrap against the golden corpus"
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
