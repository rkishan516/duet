# Phase 2b-6: the Flutter guest over duet-protocol — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Dart guest and Rust share one store over a real Flutter platform channel, and two live guests (webview + Flutter) provably cannot disturb each other.

**Architecture:** One `BasicMessageChannel<String>` with `StringCodec` named `duet/rpc`. The host uses the raw `FlutterBinaryMessenger` primitives off `engine.binaryMessenger` — no `FlutterMethodChannel`, no codec object. Guest→host arrives at `setMessageHandlerOnChannel:binaryMessageHandler:` and is answered **inline** by invoking the supplied `FlutterBinaryReply`. Host→guest pushes use `sendOnChannel:message:`, the same call `flutter/lifecycle` already makes.

**Tech Stack:** Rust (objc2 0.6.4 `catch-all`, block2 0.6.2), Flutter/Dart, FlutterMacOS.framework.

---

## Why this shape (decided, not open)

`StringCodec` is raw UTF-8 with no envelope, so `handle_text(&str) -> String` **is** the wire shape. Measured: a Dart guest was served by `duet_webview::handle_ipc_text` **byte-for-byte unmodified** — 12 requests, `Some(Int(42))` read from Rust after a Dart write, 1 push observed by the guest.

Rejected: `FlutterMethodChannel`/`StandardMethodCodec` (would require implementing a *binary* codec in Rust — a brand-new total-decode obligation over guest bytes, for no benefit); `FlutterJSONMessageCodec` (its `decode:` ends in a live `NSAssert`, so malformed guest JSON raises an ObjC exception *outside* our handler where it cannot be correlated to a `RequestId` — a direct violation of "no panic on untrusted input"); and the "two one-way channels" fallback (written to route around an unproven reply-block invocation that is **now proven**).

## Confirmed API — read from real headers on this machine

`…/FlutterMacOS.xcframework/macos-arm64_x86_64/FlutterMacOS.framework/Headers/FlutterBinaryMessenger.h`

| Signature | Line |
|---|---|
| `typedef void (^FlutterBinaryReply)(NSData* _Nullable reply);` | 21 |
| `typedef void (^FlutterBinaryMessageHandler)(NSData* _Nullable message, FlutterBinaryReply reply);` | 30 |
| `typedef int64_t FlutterBinaryMessengerConnection;` | 32 |
| `- (void)sendOnChannel:(NSString*)channel message:(NSData* _Nullable)message;` | 67 |
| `- (FlutterBinaryMessengerConnection)setMessageHandlerOnChannel:binaryMessageHandler:` | 92 |
| `- (void)cleanUpConnection:(FlutterBinaryMessengerConnection)connection;` | 103 |

Engine-source facts (framework `Info.plist` `FlutterEngine = 72d04ce778…` matches the local engine tree HEAD, so these are authoritative for *this* binary):
- The handler's `NSData` is built `dataWithBytesNoCopy:…freeWhenDone:NO` — **borrowed bytes**. Copy before use; never let the slice escape the handler.
- The `NSData` is **nil** when `message_size == 0`.
- The handler is invoked **inline on the main thread**; no further hop.
- `shutDownEngine` does **not** clear `_messengerHandlers` — teardown must call `cleanUpConnection:`.
- `_messengerHandlers` is per-`FlutterEngine`, and `backend.rs` keeps one engine per `SurfaceId`, so reusing the name `duet/rpc` across surfaces cannot cross-wire traffic.
- `makeBackgroundTaskQueue` is `@optional` with a TODO saying macOS lacks background platform channels — **do not call it**.

block2 0.6.2: block invoke thunks are `unsafe extern "C-unwind"`, so a Rust panic unwinds into ObjC. Every handler body must be wrapped in `catch_unwind`. Block closures carry **no** `Send`/`Sync` bound — capturing only a `StoreHandle` (which is `Send + Sync`) is sound, but that soundness is a convention the type system does not check, so say so in a comment.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/duet-protocol/src/text.rs` | **New.** `handle_text`, `push_text`, private `decode` + `FALLBACK_FAILURE`. Transport-agnostic. |
| `crates/duet-webview/src/lib.rs` | Keeps only `response_script`/`push_script`; delegates to `duet_protocol::handle_text`. |
| `fixtures/duet_guest/lib/duet_client.dart` | **New, tracked.** The Dart guest client. |
| `fixtures/duet_guest/lib/duet_value.dart` | **New, tracked.** Tagged-value encode/decode. |
| `fixtures/duet_guest/test/*.dart` | **New, tracked.** Dart unit tests incl. Rust-generated goldens. |
| `crates/duet-backend-macos/src/flutter_surface.rs` | **New.** `FlutterSurface`: register, reply, push, teardown. |
| `crates/duet-backend-macos/examples/flutter_state.rs` | **New.** The proof: Dart writes, Rust reads; Rust writes, Dart sees a push. |
| `crates/duet-backend-macos/examples/two_guests.rs` | **New.** The isolation proof: a webview and a Flutter engine live at once. |

---

## Task 1: Move the text-level handler into duet-protocol

**Files:** Create `crates/duet-protocol/src/text.rs`; modify `crates/duet-protocol/src/lib.rs`, `crates/duet-webview/src/lib.rs`, `crates/duet-webview/Cargo.toml`.

`duet-protocol` already depends on `duet-core`, `duet-codec`, `duet-runtime` and `serde_json` with `float_roundtrip` — the exact set this code needs. No new dependency.

- [ ] **Step 1: Move the code**

`git mv` is not appropriate (only part of the file moves). Cut `handle_ipc_text`, `decode`, `FALLBACK_FAILURE` from `crates/duet-webview/src/lib.rs` into a new `crates/duet-protocol/src/text.rs`, renaming the public entry point:

```rust
pub fn handle_text(store: &StoreHandle, subscriber: SubscriberId, text: &str) -> String
```

Add alongside it, for transports that do not wrap pushes in JavaScript:

```rust
/// Encodes a push as the JSON text a guest receives.
///
/// The webview wraps this in JavaScript (see `duet_webview::push_script`);
/// a Flutter guest receives it verbatim on its channel.
pub fn push_text(push: &Push) -> String
```

Re-export both from `crates/duet-protocol/src/lib.rs`. Move the tests that cover them too — they belong with the code.

- [ ] **Step 2: Point duet-webview at it**

`duet_webview::handle_ipc_text` is now a thin re-export (`pub use duet_protocol::handle_text as handle_ipc_text;`) **or** delete it and update callers. Prefer deleting it and updating `crates/duet-backend-macos/src/webview.rs` to call `duet_protocol::handle_text` — one name for one thing. `duet-webview` keeps `bootstrap`, `response_script`, `push_script`, and gains a `duet-protocol` dependency if it does not already have one.

Update `push_script` to build on `push_text` rather than re-encoding.

- [ ] **Step 3: Verify nothing changed behaviourally**

Run: `cargo test --workspace --exclude duet-backend-macos --locked -- --test-threads=1`
Expected: same count as before the move (374), 0 failures. A move must not change a single assertion.

Run: `cargo run -p duet-backend-macos --example webview_state`
Expected: `ALL PASS` — the webview guest is unaffected by where the function lives.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(protocol): move the text-level request handler into duet-protocol"
```

---

## Task 2: Land the Dart guest client as a tracked fixture

**Files:** Create `fixtures/duet_guest/` (a Flutter package: `pubspec.yaml`, `lib/duet_client.dart`, `lib/duet_value.dart`, `test/duet_client_test.dart`, `test/rust_goldens_test.dart`).

A working, reviewed client already exists at
`/private/tmp/claude-501/-Users-kishan-dev-rkishan516-tauri-flutter/772ffd0c-d597-4e59-be62-0128f9c7f5f7/scratchpad/duet_probe_app/lib/` and its tests at `…/scratchpad/duet_client_check/test/`. **Read those and port them** — do not re-derive. They are measured working against a real engine.

Two required edits while porting:
1. The channel name becomes **`duet/rpc`** (the scratch used `duet/protocol`). Define it once as a named constant.
2. Keep every `file:line` citation in the comments accurate after Task 1's move — `handle_ipc_text` is now `duet_protocol::handle_text` in `crates/duet-protocol/src/text.rs`.

- [ ] **Step 1: Port the client and its tests**

- [ ] **Step 2: Run the Dart tests**

Run: `cd fixtures/duet_guest && /Users/kishan/dev/rkishan516/flutterDC/bin/flutter test`
Expected: all pass. Report the actual count.

- [ ] **Step 3: Assert the two wire rules that bind Dart to Rust**

The client must reject what Rust rejects. Ensure tests cover, with these exact cases:
- `id` must be a **canonical** decimal string: `"007"` and `"+1"` are rejected (Rust's `wire.rs` rejects them; `int.tryParse` alone accepts `"007"`, so a naive client would accept ids Rust refuses).
- `Int` travels as a decimal string, never a JSON number.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(fixtures): add the Dart guest client for duet-protocol"
```

---

## Task 3: FlutterSurface

**Files:** Create `crates/duet-backend-macos/src/flutter_surface.rs`; modify `src/lib.rs`, `src/engine.rs` (expose the messenger if needed).

This is the measured-working shape. It ran against a real engine; **do not invent an alternative**:

```rust
let handler = RcBlock::new(
    move |message: *mut NSData, reply: NonNull<DynBlock<dyn Fn(*mut NSData)>>| {
        // Copy immediately: the engine builds this NSData with
        // `dataWithBytesNoCopy:…freeWhenDone:NO`, so the bytes are borrowed
        // and must not outlive this call. `message` is null for an empty
        // payload.
        let text = if message.is_null() {
            String::new()
        } else {
            let data: &NSData = unsafe { &*message };
            String::from_utf8_lossy(&data.to_vec()).into_owned()
        };
        let out = duet_protocol::handle_text(&handler_store, subscriber, &text);
        let payload = NSData::with_bytes(out.as_bytes());
        let reply_block: &DynBlock<dyn Fn(*mut NSData)> = unsafe { reply.as_ref() };
        reply_block.call((Retained::as_ptr(&payload) as *mut NSData,));
    },
);

let channel = NSString::from_str(DUET_RPC_CHANNEL);
let connection: i64 = unsafe {
    msg_send![&messenger, setMessageHandlerOnChannel: &*channel, binaryMessageHandler: &*handler]
};
```

Requirements beyond the sketch:

- [ ] **Step 1: Wrap the whole handler body in `catch_unwind`**

block2's invoke thunk is `extern "C-unwind"`; a panic crossing it is undefined behaviour in practice. On a caught panic, reply with a **`const &str`** payload — no formatting, no allocation — so the recovery path cannot itself panic. Wrap that reply in its own `catch_unwind` too.

- [ ] **Step 2: Cap the inbound message size**

Reject anything over a named constant (start at 1 MiB) **before** decoding, replying with a `Failed`. A guest must not be able to make the host allocate without bound.

- [ ] **Step 3: Teardown**

`FlutterSurface` must own its `FlutterBinaryMessengerConnection` and call `cleanUpConnection:` on drop **before** the engine shuts down — `shutDownEngine` does not clear `_messengerHandlers`. Document the order and why.

- [ ] **Step 4: `push`**

```rust
pub fn push(&self, note: &Notification) -> Result<(), BackendError>
```

Takes a `Notification`, **not** a `Push`, and **filters on `note.subscriber == self.subscriber`**, returning `Ok(())` without sending when it does not match. This is the confidentiality boundary in code rather than in a caller's discipline — a caller that forgets the check must not be able to leak another guest's notification.

- [ ] **Step 5: Tests**

`FlutterSurface` needs a live engine, so it cannot be unit-tested. **Do not write a test that only constructs types.** The proof is Task 4. Do write a unit test for the size cap and for the subscriber filter if either can be exercised without an engine — if not, say so plainly.

- [ ] **Step 6: Verify and commit**

```bash
cargo build -p duet-backend-macos && cargo clippy -p duet-backend-macos --all-targets --locked -- -D warnings
git add -A && git commit -m "feat(backend-macos): add a FlutterSurface over the binary messenger"
```

---

## Task 4: The proof — a Dart guest shares the store

**Files:** Create `crates/duet-backend-macos/examples/flutter_state.rs`; build the fixture app.

Mirror `examples/webview_state.rs`: staged, asserting **exact** values, printing PASS/FAIL per assertion and `std::process::exit(1)` on any failure, with a deadline so a hang cannot wedge anything.

It must assert:
1. A Dart `set("counter", Int(42))` is readable from Rust as **exactly** `Value::Int(42)`.
2. A Rust write of `Int(99)` reaches the guest as **exactly one** push carrying 99.
3. **An `f64` round-trips.** No `f64` has ever crossed a Flutter channel in this project — `float_roundtrip` has zero coverage on this transport. Use a value with a long decimal representation (e.g. `0.1 + 0.2`, or `f64::MAX`), write it from Dart, read it in Rust, and assert bit-exact equality via `to_bits()`.
4. Hostile input over the **real** channel — empty, non-JSON, a 1 MB parseable-but-unresolvable path — each yields a bounded `failed` reply. Assert a named byte bound.

- [ ] **Step 1: Build the fixture app**

The example needs an `App.framework` containing the Dart client. Document the exact command in the example's `//!` doc so re-running is mechanical:

```bash
cd fixtures/duet_guest && /Users/kishan/dev/rkishan516/flutterDC/bin/flutter build macos --debug
```

- [ ] **Step 2: Write and run the example**

```bash
DUET_APP_FRAMEWORK_PATH=fixtures/duet_guest/build/macos/Build/Products/Debug/App.framework \
  cargo run -p duet-backend-macos --example flutter_state
```

Report the verbatim output.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(backend-macos): prove a Dart guest shares the store over a platform channel"
```

---

## Task 5: The isolation proof — two live guests

**Files:** Create `crates/duet-backend-macos/examples/two_guests.rs`.

**This is the highest-value item in the phase and must not be cut if the increment runs long.** It is the first moment in this project's history that two guests exist at once, and it is what actually validates the guest-isolation fixes just merged (scoped `unsubscribe`, and `Request::Subscribe` carrying no `SubscriberId`).

Boot a Flutter engine **and** a `wry` webview against one `Runtime`, each with its own `SubscriberId`, then assert:

1. Both guests can read and write the shared store.
2. A write by one is visible to the other.
3. **Each guest receives only its own notifications** — subscribe both to the same path, write once, and assert each guest's push count and payload independently.
4. **Guest B cannot unsubscribe guest A.** Have the webview attempt `unsubscribe` across the id range `0..10`, then assert the Flutter guest's subscription **still delivers** a subsequent push. Assert on delivery, not merely on a count, so the test cannot pass because nothing was written.

Point 4 is a regression guard for a real, reproduced vulnerability: before the fix, `>>> guest A subscriptions remaining: 0`.

- [ ] **Step 1–3: Write, run, commit**

```bash
git add -A && git commit -m "feat(backend-macos): prove two live guests cannot disturb each other"
```

---

## Task 6: Findings and workspace verification

- [ ] Append a Phase 2b-6 section to `crates/duet-backend-macos/FINDINGS.md`, matching its existing voice and rigour.
- [ ] Record: the channel decision and why the alternatives were rejected; that Rust invoking the incoming `FlutterBinaryReply` block is now **measured** (it was the phase's largest unproven mechanism, declined by Spike C); the borrowed-`NSData` hazard; the teardown order; the f64 result.
- [ ] State plainly what could **not** be verified: nothing was seen on a display; real input remains unproven; only a debug/JIT `App.framework` was ever run; every ordering observation is conditional on the macOS embedder's merged UI/platform thread mode, which Windows and Linux do not share.
- [ ] Note honestly that `fixtures/**` and the macOS examples are **not** CI-gated — CI is ubuntu-only and excludes `duet-backend-macos`. Add `fixtures/**` to the workflow's `on.push.paths` so a fixture edit at least triggers the Rust gates, and say that the examples are recorded evidence, not regression guards.

- [ ] **Full gate — report ACTUAL output for each**

```bash
cargo test --workspace --exclude duet-backend-macos --locked -- --test-threads=1
cargo test -p duet-backend-macos
cargo clippy --workspace --exclude duet-backend-macos --all-targets --locked -- -D warnings
cargo clippy -p duet-backend-macos --all-targets --locked -- -D warnings
cargo doc --workspace --exclude duet-backend-macos --no-deps --locked
cargo fmt --all -- --check
cargo llvm-cov --workspace --exclude duet-backend-macos --locked --fail-under-lines 90
cargo run -p duet-backend-macos --example webview_state
cargo run -p duet-backend-macos --example lifecycle
```

- [ ] **Commit**

```bash
git add -A && git commit -m "docs(flutter): record Phase 2b-6 findings"
```

---

## Done criteria

- [ ] A Dart guest writes a value Rust reads back as an exact `Value`
- [ ] A Rust write reaches the Dart guest as exactly one push with the exact payload
- [ ] An `f64` round-trips bit-exactly through the Flutter transport
- [ ] Hostile input over the real channel yields bounded `failed` replies
- [ ] Two live guests coexist; neither sees the other's notifications
- [ ] A guest cannot unsubscribe another guest's subscription (regression guard)
- [ ] `duet-core` remains zero-dependency
- [ ] Full workspace gate green; both pre-existing examples still pass
- [ ] Findings record what was measured **and** what could not be verified
