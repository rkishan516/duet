# Changelog

## 0.2.0

The typed runtime, behind a new `duet-protocol/typed` entry point. Additive:
the package root and `duet-protocol/wry` are unchanged, and a guest that only
speaks the wire pays nothing for a layer it does not import. Still zero runtime
dependencies.

- `duetValueAt` and `duetValueWith` — read and functionally update a value at a
  path. Both iterative and both total: a locally built tree far past the wire's
  depth limit is handled rather than throwing `RangeError: Maximum call stack
  size exceeded`.
- `duetMergeMirror` — the three-case merge of one host patch into one watcher's
  mirror. A patch may name a path *at*, *below* or *above* the watched one,
  because `Store::set` always sends the path that was written. Kept a pure
  function so it is testable without a client.
- `DuetCodec<T extends {}>` — the seam between a TypeScript type and a
  `DuetValue`. The non-nullable bound is load-bearing: it is what lets `decode`
  answer `null` for "refused" without colliding with "decoded to null".
- `DuetReading` with four arms — `present`, `none`, `absent` and `mismatch`. A
  type mismatch is a first-class outcome, not an exception, because another
  guest may write any type to any path and a push has no call stack to throw
  into.
- `DuetRouter` — one owner of the client's push slot, id-keyed routing, a
  bounded early-arrival buffer, a refetch when a patch cannot be merged
  locally, and a resync when a value will not decode.
- `DuetField<T>` and `DuetOptionalField<T>` — typed `get`/`set`/`watch` at one
  fixed path, keeping `None` and "no such path" apart end to end.

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
