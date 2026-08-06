# Changelog

## 0.1.0

First release. The TypeScript peer of `packages/duet` (Dart) and the
`duet-core` / `duet-codec` / `duet-protocol` crates (Rust).

- `DuetValue` and the tagged-JSON value codec, mirroring `duet_core::Value` and
  `duet-codec`. Integers are `bigint`, because the wire's domain is `i64` and a
  `number` starts skipping odd integers at 2^53 — the corpus pins
  `9007199254740993` for exactly that reason.
- `DuetPath`, mirroring `duet_core::Path` — including the rule that an index may
  not follow a `.` (`a.[0]` is refused, `a[0]` is not). List indices are bounded
  by `Number.MAX_SAFE_INTEGER` rather than by `usize`, because a larger index
  would round and render back as a *different* path.
- `DuetRequest`, `DuetResponse`, `DuetPush` and the envelope codec, mirroring
  `duet-protocol`. Wire ids are `bigint`, so `i64::MAX` is representable.
- `DuetClient` over a two-member `DuetTransport` interface, matching the Dart
  package's seam so the whole protocol is testable with a dozen lines of fake.
- `duet-protocol/wry`: a transport over `window.ipc.postMessage`. `wry`'s IPC is
  one-way, so this transport builds its own correlation on the envelope's `id`,
  and bridges the host's *parsed object* replies back to text with a replacer
  that preserves negative zero — `JSON.stringify(-0)` is `"0"`, which would drop
  the sign with no error.
- A hand-written base64 codec rather than `atob`/`Buffer`: `atob` accepts input
  this format must refuse (missing padding, non-canonical trailing bits), and
  `Buffer` does not exist in a webview.
- A canonical JSON writer rather than `JSON.stringify` on objects.
  `JSON.stringify` emits integer-like keys first in ascending numeric order, so
  a plain object cannot express a map whose keys look like array indices in
  canonical order. Map values are `Map`s and the writer sorts as it emits.
- A `MAX_JSON_DEPTH` guard on decoding, checked **iteratively**. `JSON.parse`
  has no recursion limit — V8 accepted a 100 000-deep document in testing —
  where `serde_json` refuses past 128, so without this a JavaScript guest
  accepted messages every Rust peer rejects and then handed them to a recursive
  decoder.
- Conformance against `corpus/wire-corpus.json`: all 50 accept cases and all 20
  reject cases, with reject cases checked against the recorded reason code
  rather than merely "threw", and the case totals pinned.
- Differential conformance against `BOOTSTRAP_HTML` in
  `crates/duet-webview/src/bootstrap.rs`: the hand-written webview guest is
  loaded out of the Rust source, run in a `node:vm` sandbox, and checked against
  the same corpus for the subset it can reach.
