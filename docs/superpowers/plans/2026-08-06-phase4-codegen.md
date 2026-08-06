# Phase 4: Codegen — `#[derive(SharedState)]` and typed guest clients

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust developer writes one type definition and gets typed, ergonomic clients in Rust, Dart and TypeScript, all speaking the existing proven wire format.

**Architecture:** A schema JSON file is the contract — not the Rust types, and not the emitters. Rust types → schema is gated by `cargo test`. Schema → Dart/TS is gated by `duet generate --check`. Two independent gates, neither able to bless the other.

---

## Three decisions everything follows from

1. **Every path in generated code is a compile-time string literal**, minted once and validated at codegen time. Generated code never builds a `Path` from a runtime value, which makes `Path::from_segments`' trusted-construction hazard ([path.rs:38](crates/duet-core/src/path.rs:38)) structurally unreachable, and makes goldens greppable for the exact wire string.
2. **The schema file is the contract.** Two independent producers must satisfy it: a human (hand-written fixtures) and later the derive.
3. **Generated code contains no logic a reviewer cannot check by reading the diff.** All algorithms — patch merge, push routing, decode totality — live in hand-written runtime code. Emitters produce declarations and literals only.

## Why the emitters come before the derive

If the macro came first, the schema format would be whatever the macro happened to emit, the emitters would be tested against that, and nothing would independently check either. Hand-writing the schema first makes it a *specification*.

---

## Increment 1 — wire corrections ✅ DONE (merged)

`MAX_JSON_DEPTH = 127` owned by `duet-codec` with an iterative pre-scan; `Store::set` refuses over-deep writes so an unencodable store is unreachable; both JS guests settle an uncorrelatable reply instead of hanging; surrogates rejected on encode *and* decode. 430 Rust / 131 Dart / 177 TS.

---

## Increment 2 — the typed runtime in the guest packages ← NEXT

**Files:** `packages/duet/lib/src/typed/*.dart`, `packages/duet-js/src/typed/*.ts` (+ a `./typed` subpath export, mirroring the existing `./wry` one).

This is the hardest correctness in the whole feature, and it ships **hand-written and hand-tested before any generator exists**. Untyped users benefit regardless.

- [ ] **`duetValueAt` / `duetValueWith`** — read and functionally update a value at a path. `duetValueWith` must be **iterative**; a recursive rebuild dies on exactly the deep input the depth limit exists to bound.

- [ ] **`DuetCodec<T extends Object>` (Dart) / `<T extends {}>` (TS)** — the bound is mandatory, not stylistic. Measured: with a nullable `T`, "decoded null" and "absent" collapse into the same `runtimeType`, so the codec cannot distinguish `Value::Null` from a missing path. The bound rejects a nullable type argument **at the definition site**. Pin that with a test that would fail if the bound were dropped.

- [ ] **`DuetField<T>` / `DuetOptionalField<T>`** — typed get/set/watch at a fixed literal path. `Option<T>` maps to `DuetOptionalField`, and the missing-path vs explicit-null distinction must be preserved end to end.

- [ ] **`DuetRouter`** — routes pushes to typed watchers. Requirements, each for a measured reason:
  - single-owner push slot (two owners silently steal each other's notifications)
  - id-keyed routing, not path-prefix scanning
  - three-case mirror merge (set at path / set above / set below)
  - bounded early-arrival buffer — a push can precede its `subscribed` reply
  - null-mirror refetch
  - **resync on decode failure**: another guest can write any type to any path, so a typed watcher *will* meet a value it cannot decode. Recover, do not throw into the app.

- [ ] **`DuetWatch`, `DuetMismatch`** — a mismatch is a first-class outcome, not an exception. The two-guest proof already has both guests writing one store; this is a real runtime state.

- [ ] **Write the `Option<Editor>` and `Option<i64>` tests FIRST**, before the interface is frozen. Measured: with `Option<Struct> = None`, a child path `get` returns null, `set` **fails** with "wrong kind of node", and `subscribe` succeeds — three different behaviours the typed layer must not paper over.

- [ ] Both packages' versions bump; both test suites stay green.

---

## Increment 3 — `crates/duet-schema` + the `duet` facade

`Schema`/`TypeDef`/`Ty`/`Registry` with **cycle detection**; the `SharedState` trait; `DecodeError`; primitive and container impls; `duet::Bytes`; identifier computation and the collision reject set; hand-rolled `Schema::render()`; `Field<T>`, `OptionalField<T>`, `TypedStore`; `duet::install`.

`SharedState` is a public, hand-implementable trait. **Rejection is "no impl exists"**, never macro-time token inspection — a derive sees tokens, never resolved types, so `type Blob = Vec<u8>` would defeat any syntactic special case. Rejections carry `#[diagnostic::on_unimplemented]` messages naming the fix.

**Compile-time rejections:** `u64`/`u128`/`i128`/`usize`/`isize` (no representation in `Value::Int`); `f32` (lossy inbound, and guests have no 32-bit float anyway); `HashSet` (nondeterministic iteration ⇒ byte-unstable goldens); non-`String`-keyed maps; borrowed types; `Rc`/`RefCell`/`Mutex` (two handles become two copies); `Box<dyn Trait>`; generics; `PathBuf`/`OsString` (WTF-8 vs UTF-8); time and UUID types (no canonical spelling — make the developer choose); `Option<Option<T>>` (`Some(None)` and `None` both lower to `Null`).

`duet-core` gains **no** dependency; the zero-dep CI assertion lands here.

---

## Increment 4 — `crates/duet-codegen` ✅ DONE

A `serde_json` schema **reader** independent of the hand-rolled writer, so a round-trip across the two is a genuine cross-check. Dart and TS emitters. Hand-written `schema/app.json`, `schema/wide.json`, `schema/negative/*.json`. Goldens committed **into** the packages. Staleness, determinism, coverage-floor, totality-fuzz and mutation-sensitivity tests.

**Shipped.** 686 Rust / 233 Dart / 279 TypeScript. `duet-codegen` at 98.4% lines.

Decisions taken, each with a test that would fail if it were reversed:

- **A wire key is never rewritten; an accessor name always is.** `snake_case` becomes `lowerCamelCase` in Dart and TypeScript alike; the path keeps the schema's own spelling. `tests/casing.rs` pins both directions, and `tests/real_host.rs` resolves every path literal in the committed goldens against a real `duet_core::Store`, so a camel-cased path fails against the host rather than against an assertion about it.
- **`schema/negative/` and `schema/unemittable/` are different directories** because they are different rejections: one is "this file is not a schema", the other is "this schema has no faithful spelling in the target languages". Collapsing them would leave a developer unable to tell a corrupt file from a type definition that needs changing.
- **Rejected at codegen time**: a non-struct root, an `optional` anywhere but a field's own type (no codec can have a nullable type argument), a key that is not an ASCII identifier, two keys that camel-case alike, a declaration-name collision, and more than 256 struct-typed paths.

What the golden tests do **not** catch, and what covers it instead: a golden proves the emitter still emits what it emitted before. `tests/real_host.rs` resolves the paths on a real store; `dart analyze` and `tsc` compile the output; and each guest package drives its generated client against a fake host transcribed from `duet-core`'s write rules. Measured by mutation: a misspelled path fails 3 tests in each guest suite, a codec swapped alone does not compile, and a type and codec swapped together fail 1 test in each guest suite with a mismatch instead of a value.

---

## Increment 5 — `corpus/schema-corpus.json` + guest conformance

Split the corpus half (CI-feasible today) from the live-host half. **This is the first moment "one type definition, three agreeing clients" is proven rather than asserted.**

---

## Increment 6 — `crates/duet-derive`

`#[derive(SharedState)]`. The verification trick: the derive over the fixture's Rust structs must produce **byte-identically** the `schema/app.json` that increments 4 and 5 already consume.

**Guard against self-blessing:** any adjustment to `schema/app.json` in this increment must be a *separate reviewed commit landing before the derive*, with the diff explained. Otherwise the derive silently redefines the specification it is supposed to satisfy.

Plus `trybuild` compile-fail cases for every rejection above.

---

## Increment 7 — `crates/duet-cli` ✅ DONE

`duet generate`, `duet generate --check`. Thin, as intended: every transformation is a `duet-codegen` call the earlier increments already gate.

**Shipped.** 1034 Rust / 414 Dart / 460 TypeScript. `duet-cli` at 97.95% lines; workspace total 96.14%.

Decisions taken, each with a test that would fail if it were reversed:

- **Argument parsing is hand-rolled, not `clap`.** One subcommand and four flags did not clear the bar this workspace sets for a dependency — `duet-core` has none by design and CI asserts it, `packages/duet-js` has no runtime dependency and CI asserts that too, and `duet-host-stdio` already hand-rolls its own argv. What is given up is named in `args`: completions, near-miss suggestions, coloured errors. Every parser arm is pinned, because the failure mode of hand-rolled argv is not a crash but accepting something and quietly doing the wrong thing.
- **`--dart` and `--ts` are each optional; at least one is required.** A run that generated nothing would exit 0 and look exactly like a run that succeeded — the worst outcome a generator has.
- **Four exit codes.** `3` for a stale file is distinct from `1` for a failed run, so a CI job can tell a schema that no longer parses from a client that merely needs regenerating.
- **The regeneration command is canonical, not argv-shaped, and never carries `--check`.** It is written into every generated header *and* printed by a failing `--check`, so flag order cannot change the bytes and the printed fix is never the invocation that writes nothing.
- **Writing is two-phase**: every file staged to a temporary beside its target, nothing renamed until all are staged. A target that is itself a directory (`--dart lib/`) is pre-flighted, because it is the one mistake that stages cleanly and fails at the rename — a test caught the first version replacing the Dart client before objecting to the TypeScript one.

The CI `--check` step runs against `examples/generated/`, not the committed goldens under `packages/*/test/generated/`: those override the TypeScript import specifiers to reach into `packages/duet-js/src`, since they live inside that package and it cannot import itself by name before a build. `examples/generated/` uses the **defaults**, so the step is the only thing in CI exercising the published `duet-protocol` specifiers a third party gets.

---

## Deferred, deliberately

**Phase 4b** (pure extension of a proven pipeline): narrowing integer types, `char`, `[T; N]`, `BTreeSet`, tuples, data enums, `Cow`, custom guest types.
**Phase 5:** `duet dev` / hot reload (Spike C already retired the risk at median 123.3 ms, so shipping it late costs no project risk), `#[command]` RPC codegen, collection handles, reactive adapters, per-subscription sequence numbers. — *hot reload shipped 2026-08-06; see `2026-08-06-phase5-hot-reload.md`. The rest is still deferred.*

---

## The MVP

**Increments 2 → 6.** Not 2–5: without the derive the developer hand-writes schema JSON, which is not "one type definition". Not 2–4: three emitters agreeing with their own goldens proves nothing. Increment 7 is required before an external *user*, not before the *claim*.

## Done criteria

- [x] One `#[derive(SharedState)]` type yields typed Rust, Dart and TypeScript clients
- [x] The derive's schema output is byte-identical to the hand-written specification
- [x] Generated clients are proven against a real host over the real wire, not only against goldens
- [x] Every rejection has a `trybuild` case and a message naming the fix
- [x] A stale schema or stale generated file fails CI — now four ways, the fourth being `duet generate --check`, the only one a user outside this workspace can run
- [x] `duet-core` remains zero-dependency
