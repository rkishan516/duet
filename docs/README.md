# Duet documentation

Duet is a framework for desktop applications whose UI is rendered partly by
Flutter and partly by web technology, in separate top-level OS windows, where
either renderer can be destroyed to reclaim its memory while the application
keeps running. A Rust process is the **host**: it owns the windows and the
application state. A Flutter engine and a `wry` WebView are **guests** that
attach, read and write one shared store, and detach. Neither owns state, and
neither talks to the other.

One sentence decides the hard cases, and it is worth carrying into every document
below: **state survives teardown; events don't**
(`crates/duet-core/src/lib.rs:16`).

These eight chapters are written for a developer who has never seen Duet. Every
structural claim carries a `file:line` citation, every code sample is copied from
or checked against real source, and measured numbers are attributed to the run
that produced them. Where something has not been verified, the text says so
rather than rounding up.

---

## Contents

| # | Document | The question it answers |
|---|---|---|
| 01 | [What Duet is and why it exists](01-overview.md) | Why does this framework exist, what does it look like end to end, and what do I actually write? |
| 02 | [How the pieces fit](02-architecture.md) | Which code runs on which thread, where are the seams, and why can almost all of it be tested with no display? |
| 03 | [The shared store](03-state.md) | What is a `Value`, how do paths address it, who gets notified by a write, and what are the four states a path can be in? |
| 04 | [Surfaces, teardown and reclaiming memory](04-lifecycle.md) | When is a renderer destroyed, what actually gives the memory back, and what survives going cold? |
| 05 | [The wire format](05-wire-protocol.md) | What exact bytes cross the boundary, what must a conforming client accept and refuse, and why those rules? |
| 06 | [Typed clients and commands](06-codegen-and-commands.md) | How does one Rust type become agreeing Dart and TypeScript clients, and how does a guest ask the host to run a function? |
| 07 | [Hot reload](07-hot-reload.md) | What does `duet dev` do on every save, how long does it take, and why does nothing get lost? |
| 08 | [How this project proves things](08-testing.md) | What is the evidence behind the claims, and what has this codebase learned about tests that cannot fail? |
| 09 | [Known limitations](09-limitations.md) | What does Duet *not* do yet, what could not be verified, and what was deferred on purpose? |

### Also in this directory

| Path | What it is |
|---|---|
| [`superpowers/specs/2026-08-04-duet-design.md`](superpowers/specs/2026-08-04-duet-design.md) | The original design specification. **History, not truth** — it predates the spikes, and where it disagrees with the code the code is right. Chapter 01 §8 lists the specific contradictions. |
| [`superpowers/spikes/2026-08-04-phase0-findings.md`](superpowers/spikes/2026-08-04-phase0-findings.md) | The Phase 0 findings: what the three feasibility spikes established before any framework code was written. |
| [`superpowers/plans/`](superpowers/plans/) | Per-phase implementation plans. Useful mainly for the review checklists at the top of each, which accumulate the lessons chapter 08 §5 is about. |

### Where the primary evidence lives

The documents cite these constantly; they are the record, not a summary of it.

| Path | What it records |
|---|---|
| `crates/duet-backend-macos/FINDINGS.md` | Every measurement taken against a real Flutter engine and a real webview, including the verdicts that say "cannot verify here" |
| `spikes/spike-a-macos/FINDINGS.md` | The original RSS measurements: what a booted engine costs and what teardown gives back |
| `spikes/spike-b-macos/FINDINGS.md` | Run-loop coexistence between `tao`, Flutter and a `WKWebView` |
| `spikes/spike-c-macos/FINDINGS.md` | Hot reload in a custom embedder, including the `force: true` crash and the two false starts before it |
| `corpus/wire-corpus.json` | 63 accept and 37 reject cases, consumed by Rust, Dart and TypeScript |
| `corpus/schema-corpus.json` | Per schema, the seed the stdio host starts from and every path a generated client binds |
| `.github/workflows/duet.yml` | Every gate, with the reasoning in its comments |

---

## Reading paths

### (a) Evaluating Duet — "is this the right shape for my problem?"

About an hour, and you can stop after the second document if the answer is no.

1. **[01 — What Duet is and why it exists](01-overview.md)**, all of it. §1 is the
   measurement the whole design follows from; §8 is the honest list of what is
   missing, including the fact that macOS is the only implemented backend.
2. **[04 — Surfaces, teardown and reclaiming memory](04-lifecycle.md)** §7 and §11.
   §7 is the memory claim with the numbers behind it; §11 says plainly what has not
   been verified on this hardware. If the reclamation story is not worth the
   constraints, this is where you find out.
3. **[03 — The shared store](03-state.md)** §7, the four states a path can be in.
   This is the shape your UI code will actually be written against, and it is the
   most likely thing to feel wrong if Duet is a bad fit.
4. **[08 — How this project proves things](08-testing.md)** §1 and §8 — what the
   evidence is, and what is out of reach on a machine with no display.

Skip 02, 05 and 06 entirely on a first pass; they are reference.

### (b) Building an application with Duet

Read in order; each chapter assumes the one before it.

1. **[01 — Overview](01-overview.md)** §2 (the governing principle) and §5 (what
   you write, and what is generated). §5 is the shortest complete answer to "what
   does my project look like".
2. **[03 — The shared store](03-state.md)** in full. The path grammar, `set`'s
   refusal to create intermediate nodes, and the four-arm reading are the three
   things that will otherwise surprise you.
3. **[06 — Typed clients and commands](06-codegen-and-commands.md)**. §2.2 and §2.3
   tell you which Rust types you may share; §2.7 is the casing rule, which is the
   one convention you must not fight. Part B when you need a host function.
4. **[04 — Lifecycle](04-lifecycle.md)** §3 (choosing a `Policy`), §8 (what
   survives) and §9 (a driver's obligations).
5. **[07 — Hot reload](07-hot-reload.md)** §1. Enough to run `duet dev` and to
   recognise the two non-failures.
6. **[02 — Architecture](02-architecture.md)** §8, the surprises table. Eight rows;
   read it before you debug something.
7. **[05 — The wire format](05-wire-protocol.md)** only if you are writing a client
   for a fourth language, or debugging raw bytes. The generated clients mean you
   never have to.

### (c) Contributing to Duet

1. **[08 — How this project proves things](08-testing.md)** *first*, especially §5.
   The review standard here is not "does your test pass" but "could your input tell
   a correct implementation from a broken one". Reading §5 before you write a test
   saves the review round trip.
2. **[02 — How the pieces fit](02-architecture.md)** in full. The dependency
   direction, the two trait seams, the reentrancy guard and the effects-as-data
   pattern are what a change has to stay inside.
3. **The chapter for the layer you are changing** — 03 for the store, 04 for the
   lifecycle, 05 for the wire, 06 for the schema or the emitters, 07 for
   `duet dev`.
4. **The relevant plan** under [`superpowers/plans/`](superpowers/plans/), for the
   review checklist at the top of it and the constraints the phase was built under.
5. **`FINDINGS.md` for the crate you are touching**, if it has one, before quoting
   any number.

Three conventions that are enforced rather than encouraged:

- `duet-core` has an empty `[dependencies]` and CI asserts it stays that way.
  Do not add one, including a dev-dependency.
- Everything except `duet-backend-macos` is `#![forbid(unsafe_code)]`, with
  `#![deny(missing_docs)]` and an `# Errors` section on every `Result`-returning
  function.
- An unobserved pass is not a pass. If you cannot run it, say so in the text
  rather than implying you did.
