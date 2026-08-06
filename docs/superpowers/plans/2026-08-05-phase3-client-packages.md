# Phase 3: Client Packages and a Shared Golden Corpus — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publishable Dart and TypeScript client packages, both proven against one golden corpus generated from Rust, all running in CI on ubuntu.

**Architecture:** Rust emits a single `wire-corpus.json` as a snapshot test. Dart and TypeScript tests consume that same file. A change to the Rust wire format regenerates the corpus, and the guest tests then fail loudly — three implementations cannot drift silently.

**Tech Stack:** Rust (`duet-codec`, `duet-protocol`), pure Dart (`dart test`), TypeScript/Node.

---

## Why this phase, and why now

Both guest clients work and are proven end to end, but neither is a *package*: the Dart client lives in a test fixture, and the JavaScript client is a Rust string constant. Neither is publishable, and CI runs neither. For a framework whose first binding decision was "reusable OSS", that is the gap that matters most — and it is a prerequisite for Phase 4's codegen, which needs real packages to generate into.

**The format is already settled.** Designing this corpus exposed four divergences between the three implementations, all now fixed and merged: non-canonical ids (a silent hang), the `-0.0` sign lost in JavaScript, an id domain wider in Rust than Dart can represent, and map key order differing between UTF-8 bytes and UTF-16 code units. The corpus can now pin something true.

---

## The witness representation — the hard part, decided

A corpus entry must say "this wire text must decode to *this value*" in a way Dart, TypeScript and Rust all check **identically**, and which a guest **cannot satisfy by echoing its input**.

Expressing the expectation as JSON of the same shape fails that second test. So the witness is a deliberately *different* encoding:

```jsonc
{"k": "null"}
{"k": "bool",  "v": true}
{"k": "int",   "v": "9007199254740993"}     // decimal string: exact past 2^53
{"k": "float", "bits": "8000000000000000"}  // IEEE-754 bits, hex, big-endian
{"k": "str",   "utf8": [104, 105]}          // UTF-8 bytes: no escaping ambiguity
{"k": "bytes", "hex": "666f6f"}
{"k": "list",  "items": [ /* witnesses */ ]}
{"k": "map",   "entries": [["key", /* witness */]]}  // code-point order
```

Two choices carry the weight:

- **Floats are hex bits, never decimal.** `-0.0`, `NaN`, subnormals and `0.1` all have exact, unambiguous witnesses, and float *text* is not comparable across languages anyway (`1e16` is `1e+16` in Rust and `10000000000000000` in JS).
- **Strings are UTF-8 byte arrays.** Escaping, normalisation and surrogate handling can't creep in.

A guest passes only by genuinely decoding and then re-deriving these facts.

---

## Corpus schema

```jsonc
{
  "version": 1,
  "generator": "cargo test -p duet-protocol --test wire_corpus -- --ignored",
  "accept": [
    {
      "name": "value/int/above_2_53",
      "layer": "value",                     // value | request | response | push
      "wire": "{\"t\":\"i\",\"v\":\"9007199254740993\"}",
      "witness": {"k": "int", "v": "9007199254740993"},
      "reencodes_to": null,                 // null = re-encodes to `wire` itself
      "reencode_byte_exact": true           // false when any float is a JSON number
    }
  ],
  "reject": [
    {
      "name": "envelope/id/non_canonical",
      "layer": "request",
      "wire": "{\"kind\":\"get\",\"id\":\"007\",\"path\":\"a\"}",
      "reason": "bad_int"
    }
  ]
}
```

`reencode_byte_exact` is **computed by Rust**, true iff the canonical encoding contains no JSON-number float anywhere. Guests assert byte equality when it is true and deep structural equality when it is false. This catches a guest whose encoder and decoder are wrong in the *same* direction — which a self-inverting round-trip test cannot.

---

## Task 1: `reason_code` in duet-codec

**Files:** modify `crates/duet-codec/src/error.rs`, `crates/duet-codec/src/lib.rs`.

`CodecError` is `#[non_exhaustive]`. A match written outside `duet-codec` would need a wildcard arm, which would silently map a future variant to a wrong reason — reintroducing the exact "rejected for a new, wrong reason" failure the corpus exists to catch. Inside the defining crate the match is exhaustive and a new variant is a **compile error**.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_error_variant_has_a_distinct_reason_code() {
    // The corpus asserts WHY a message was rejected, not merely that it was.
    // A prototype measured the hole this closes: under a mutated codec all
    // reject cases still "passed" — rejected for a new, wrong reason.
    let all = [
        CodecError::UnknownTag(String::new()),
        CodecError::BadShape(String::new()),
        CodecError::BadInt(String::new()),
        CodecError::BadFloat(String::new()),
        CodecError::BadBase64(String::new()),
        CodecError::BadPath(String::new()),
    ];
    let codes: Vec<&str> = all.iter().map(CodecError::reason_code).collect();
    let unique: std::collections::BTreeSet<&&str> = codes.iter().collect();
    assert_eq!(unique.len(), codes.len(), "reason codes must be distinct: {codes:?}");
    assert!(codes.iter().all(|c| c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')));
}
```

- [ ] **Step 2: Run it, watch it fail** — `cargo test -p duet-codec` → no method `reason_code`.

- [ ] **Step 3: Implement**

```rust
impl CodecError {
    /// A stable, machine-readable reason code for the golden corpus.
    ///
    /// Lives here rather than in a consumer because `CodecError` is
    /// `#[non_exhaustive]`: a match outside this crate would need a wildcard
    /// arm and would silently give a future variant a wrong code. Here the
    /// match is exhaustive and a new variant fails to compile.
    ///
    /// Note `bad_json` is also a corpus reason but has no variant: input that
    /// fails `serde_json` parsing never reaches `CodecError` at all.
    pub fn reason_code(&self) -> &'static str {
        match self {
            CodecError::UnknownTag(_) => "unknown_tag",
            CodecError::BadShape(_) => "bad_shape",
            CodecError::BadInt(_) => "bad_int",
            CodecError::BadFloat(_) => "bad_float",
            CodecError::BadBase64(_) => "bad_base64",
            CodecError::BadPath(_) => "bad_path",
        }
    }
}
```

- [ ] **Step 4: Verify and commit** — `cargo test -p duet-codec`, then:

```bash
git add crates/duet-codec && git commit -m "feat(codec): add stable reason codes for the golden corpus"
```

---

## Task 2: The corpus generator and its snapshot test

**Files:** create `crates/duet-protocol/tests/wire_corpus.rs`, `corpus/wire-corpus.json`.

The corpus lives at the **repo root** in `corpus/`, not under any one language's tree — all three consume it as peers.

- [ ] **Step 1: Write the generator as an ignored test**

A `#[test]` (not an example or xtask) so it needs no new binary and no new dependency. Two entry points:

- `#[test] fn corpus_matches_the_committed_file()` — regenerates in memory and asserts byte equality with `corpus/wire-corpus.json`. **Runs in CI.** On mismatch, print the exact regeneration command.
- `#[test] #[ignore] fn regenerate_corpus()` — writes the file. Run by hand.

- [ ] **Step 2: Populate the corpus exhaustively**

Every case below must appear. This list is the deliverable; a gap here is a gap in all three languages at once.

**Values — accept:** null; `true`; `false`; ints `0`, `1`, `-1`, `i64::MIN`, `i64::MAX`, `9007199254740993` (2^53+1); floats `0.0`, `-0.0` (sentinel `"-0"`), `1.0`, `0.1`, `1e16`, `f64::MAX`, `5e-324` (subnormal), `NaN`, `Infinity`, `-Infinity`; strings empty, ASCII, `"café ✓ 😀"`, one containing `"` `\` and a newline, one containing U+0000–U+001F; bytes empty, `"foo"`, one that exercises base64 padding at each length mod 3; empty list, empty map, nested list-in-map-in-list; **a map with keys U+E000, U+FFFD, U+1F600** (pins code-point order — a BMP-only map would pass even with the bug).

**Envelope — accept:** every `Request` variant; every `Response` variant; `Push`; id `0`; id `i64::MAX`.

**Reject:** ids `"007"`, `"+1"`, `"1 "`, `""`, a numeric (non-string) id, `"9223372036854775808"` (above the domain); unknown tag; missing `t`; missing `v`; `i` payload as a JSON number; `f` payload as an unrecognised sentinel string; non-base64 `b` payload; unparseable path `"a.[0]"`; malformed JSON (`bad_json`); 200-level array nesting (`bad_json` — serde_json's recursion limit fires before the codec is reached).

Each reject entry carries its `reason`. Rust asserts the reason; guests assert only that rejection happened via the client's own error type — a deliberate, documented gap.

- [ ] **Step 3: Assert the corpus is self-consistent in Rust**

```rust
#[test]
fn rust_satisfies_its_own_corpus() {
    // If Rust cannot pass the corpus it generated, the corpus is wrong and
    // every guest would be chasing a phantom.
    let corpus = load();
    for case in &corpus.accept {
        let decoded = decode_for(case).unwrap_or_else(|e| panic!("{}: {e}", case.name));
        assert_eq!(witness_of(&decoded), case.witness, "{}", case.name);
        let target = case.reencodes_to.as_deref().unwrap_or(&case.wire);
        assert_eq!(encode_for(&decoded), target, "{} must re-encode canonically", case.name);
    }
    for case in &corpus.reject {
        match decode_for(case) {
            Ok(v) => panic!("{} should have been rejected, got {v:?}", case.name),
            Err(e) => assert_eq!(reason_of(&e), case.reason, "{} rejected for the wrong reason", case.name),
        }
    }
}
```

- [ ] **Step 4: Prove the guard has teeth**

Temporarily change one encoding in `duet-codec` (e.g. drop the `-0` sentinel), run `corpus_matches_the_committed_file`, and confirm it **fails**. Revert. **Report the observed failure output** — a snapshot test that cannot fail is worse than none.

- [ ] **Step 5: Commit**

```bash
git add corpus crates/duet-protocol && git commit -m "feat(protocol): generate a cross-language golden wire corpus"
```

---

## Task 3: `packages/duet` — the pure-Dart package

**Files:** create `packages/duet/` (pubspec, LICENSE, README, CHANGELOG, `lib/duet.dart`, `lib/src/*.dart`, `test/*.dart`).

Measured: `dart test` **cannot** load `package:flutter/services.dart` — a hard compile failure, not a lint. Splitting the transport out puts the entire wire surface behind plain `dart test`, so CI needs Dart (seconds) rather than a full Flutter SDK.

- [ ] **Step 1: Extract, replacing the Flutter dependency with a seam**

Move `duet_value.dart` (already pure — imports only `dart:convert`) and `duet_client.dart` from `fixtures/duet_guest/lib/`. Replace `BasicMessageChannel` with a two-member transport interface:

```dart
/// The transport a [DuetClient] speaks over. `duet_flutter` implements this
/// with a BasicMessageChannel; a browser or test harness can implement it
/// with anything that carries text both ways.
abstract interface class DuetTransport {
  /// Sends one request and completes with the host's reply, or null if no
  /// host is listening.
  Future<String?> send(String request);

  /// Installs the handler for unsolicited host pushes. Null removes it.
  set onPush(void Function(String message)? handler);
}
```

Keep every existing test, adapted to a `FakeTransport`. The driver files (`main.dart`, `*_driver.dart`, `guest_support.dart`) stay in the fixture — they are not the client.

- [ ] **Step 2: Consume the corpus**

`test/wire_corpus_test.dart` reads `../../corpus/wire-corpus.json` and, for every case:
- accept → decode, compare to the witness, then re-encode and assert **byte equality** when `reencode_byte_exact` is true, deep structural equality otherwise
- reject → assert it throws, and throws only the client's own error type

Build the witness comparison from the decoded value — **never** by re-reading the wire text.

- [ ] **Step 3: Verify**

```bash
cd packages/duet && dart pub get && dart test && dart analyze && dart pub publish --dry-run
```

Report the actual test count and the dry-run output.

- [ ] **Step 4: Commit**

---

## Task 4: `packages/duet_flutter` — the binding, and fixture migration

- [ ] Implement `DuetTransport` over `BasicMessageChannel<String>('duet/rpc', StringCodec())`. This should be a few dozen lines.
- [ ] Point `fixtures/duet_guest` at both packages via path dependencies (`pubspec_overrides.yaml`), delete the duplicated client sources, and confirm **all three macOS examples still pass** after `flutter build macos --debug`. That is the real regression test for this refactor.
- [ ] `dart pub publish --dry-run` for both packages; report output.

---

## Task 5: `packages/duet-js` — the TypeScript package

- [ ] TypeScript source; the client is transport-agnostic like Dart's, with a `wry` adapter over `window.ipc.postMessage`.
- [ ] Consume the same corpus, with the same accept/reject assertions.
- [ ] **Resolve the bootstrap relationship explicitly:** `BOOTSTRAP_HTML` is currently hand-written in Rust. Either the built JS is committed and Rust `include_str!`s it with a CI staleness check, or the bootstrap stays a minimal built-in and the drift risk is *named and tested*. Pick one, implement it, and state the reasoning.
- [ ] Test runner: prefer `node --test` (zero dependencies) unless something genuinely requires more.
- [ ] Note the known JS limitation: `JSON.stringify` emits integer-like keys first in ascending numeric order, so a map with such keys cannot be emitted in canonical order from a plain object. It still decodes correctly. Such a value must not be captured as a corpus fixture from JS.

---

## Task 6: CI

- [ ] Add ubuntu jobs running `dart test` (via `dart-lang/setup-dart`) and the TypeScript tests (via `actions/setup-node`). These are the first non-Rust tests this project has ever run in CI.
- [ ] Keep `duet-backend-macos` excluded — it links `FlutterMacOS.framework` and cannot build on ubuntu. State honestly in the findings that the three macOS examples remain recorded evidence, not regression guards.
- [ ] Verify the corpus staleness check runs in CI.

---

## Done criteria

- [ ] `corpus/wire-corpus.json` is generated by Rust and covers every case listed in Task 2
- [ ] Rust, Dart and TypeScript all assert against it — accept **and** reject cases
- [ ] Mutating a Rust encoding makes the corpus check fail (observed, not assumed)
- [ ] `packages/duet` passes `dart pub publish --dry-run`
- [ ] `packages/duet_flutter` passes `dart pub publish --dry-run`
- [ ] All three macOS examples still pass after the fixture migrates to the packages
- [ ] Dart and TypeScript tests run in CI on ubuntu
- [ ] `duet-core` remains zero-dependency
