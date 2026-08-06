# Phase 5 — `duet dev` and hot reload

**Status: ✅ DONE (2026-08-06).**

Goal: full hot-reload parity, which was a binding v1 decision at the outset, not
a stretch. Spike C had already retired the technical risk
(`spikes/spike-c-macos/FINDINGS.md`: median 123.3 ms edit-to-pixel, Dart heap
state surviving every reload); this phase productionised the recipe and proved
the *shared store* survives too.

---

## Increment 1 — `crates/duet-dev` ✅ DONE

The four pieces the spec's §8.2 diagram needs, each with the parts a throwaway
spike could skip.

| Module | What it owns | Why it is separate |
|---|---|---|
| `compiler_protocol` | the `frontend_server` line protocol | reverse-engineered, so the part most likely to shift under a Flutter upgrade — all of it pure, with tests for shapes this SDK has never produced |
| `frontend_server` | the child process | death, hangs, stderr, and a changed protocol are four different reports |
| `frame` | RFC 6455 framing | pure functions, so every malformed header a peer could send is a two-line test |
| `ws` | the connection | handshake, deadlines, ping/pong, close |
| `jsonrpc` | correlation | the VM service multiplexes events onto the same socket; a driver that took the next frame as its reply would break on the first GC |
| `vm_service` | the three RPCs | `reloadSources` has **no `force` field to set** |
| `watch` | debounced polling | injected clock, so the debounce is asserted rather than slept through |
| `packages` | file path → `package:` URI | library identity in the diff must match the running kernel's |
| `locate` | VM service discovery | the fd-1 replacement |
| `sdk` | the three SDK artefacts | converts "the reload hung" into "no snapshot at `<path>`" |

**Design rules, all enforced rather than documented:**

- Every `DevError` carries the `Stage` it happened at. There is no constructor
  that omits one and `DevError::stage()` is infallible.
- No `unwrap`, `expect` or `panic!` outside tests. A compile error is
  `Ok(Reload::CompileFailed)`, not `Err` — it is the most common thing that
  happens in a dev loop and it is not a failure of the driver.
- Nothing waits without a deadline.
- `force: true` on `reloadSources` is designed out, not commented against:
  `ReloadSources` has no such field, and a test asserts the encoded request has
  exactly two params.

**Two dependency decisions**, both argued in `Cargo.toml`:

- **`serde_json`** — already in `duet-codec`, `duet-protocol` and
  `duet-codegen`, therefore already in `duet-cli`'s tree. Free.
- **No WebSocket or file-watching crate.** `duet-cli` is the framework's front
  door and had exactly one dependency; `tungstenite` would add a subtree to
  every `cargo install duet-cli`. `duet-codec` set the precedent by
  hand-rolling base64. The honest cost is written down in `ws.rs`: this
  connects only to loopback, only to a Dart VM this tool or its own child
  started, and refuses anything outside the subset it implements.

165 tests, 93.7 % line coverage.

## Increment 2 — `duet dev` on the CLI ✅ DONE

```
duet dev --flutter <dir> [--flutter-root <dir>] [--entrypoint <uri>] -- <host command>
```

Everything after `--` is the host command, passed through unsplit — a host
command carries its own flags, and a path with a space must survive.

`dev` is the one arm `execute` does not buffer into an `Outcome`: a session
runs until it is interrupted, so buffering would show the developer nothing
until it ended. `dev::run` takes its two writers explicitly, which is how both
`execute` and the tests drive it.

`UsageError::UnknownFlag` and `UnexpectedArgument` gained the subcommand, so a
`generate` flag typed into `dev` gets `dev`'s flag list.

Adds no crate to `duet-cli`'s dependency tree beyond `duet-dev` itself.

## Increment 3 — the fd-1 redirect, retired ✅ DONE

Spike C's `dup2` of its own fd 1 swallows everything else the process prints,
forever, with nothing to enforce the "use `eprintln!` from now on" rule it
imposes. It would also make the trick and `duet-host-stdio` mutually exclusive.

Replaced two ways, neither touching a file descriptor:

1. **A child process's piped stdout** (`duet dev`). Falls out of the
   architecture §8.2 already describes. Auth code kept.
2. **`FLUTTER_ENGINE_SWITCHES` with a fixed port** (an in-process host).
   Measured: `vm-service-port=45671` + `disable-service-auth-codes` produced
   exactly `http://127.0.0.1:45671/`. Cost stated on the function.

`write-service-info=<path>` was tried first and is **not wired up** in this
embedder — recorded so nobody spends the same twenty minutes.

## Increment 4 — the proof ✅ DONE

`crates/duet-backend-macos/examples/hot_reload.rs`, and a third driver in
`fixtures/duet_guest` — the only one that calls `runApp`, because it is the only
one whose claim is about a rendered frame rather than about the transport.

Ten iterations, three runs, all PASS:

- every reload applied, 4 libraries received against **544 kept**
- the Dart change reached a rendered frame, 10/10
- **the Duet store's contents survived**: `hostWitness` written `Int(4242)`
  before the first reload, still `Int(4242)` after the tenth
- reload not restart: the guest's `initState`-assigned nonce unchanged, its
  frame counter monotonic

Latency, `fs::write` → the new marker readable from the Rust store having been
built into a rendered frame: **min 38.1, median 43.0, max 59.1 ms, n=30 across
three runs**. Under half Spike C's 123.3 ms median over a *longer* path,
because this fixture rebuilds three widgets rather than a `MaterialApp` with a
`Ticker`. The recompile leg — the part this crate controls — is comparable at
6–19 ms against 8.8–21.8.

"Rendered" means Flutter produced the frame in-process. This machine has no
reachable on-screen WindowServer for spawned processes.

## Increment 5 — the lifecycle RSS record, corrected ✅ DONE

Not hot reload, but found while verifying: `examples/lifecycle.rs`'s absolute
81,920 kB floor sat between two tight clusters selected by *which fixture app*
was booted (71.3–71.6 MB headless, 122.5–124.1 MB with a widget tree). Not
flaky — ambiguous. Re-baselined onto two shares, both app-independent, one of
which newly pins Spike A's actual finding that `shutDownEngine` and not detach
is what reclaims. See `crates/duet-backend-macos/FINDINGS.md` F24.

---

## Deferred, deliberately

**The web half of `duet dev`** — a Vite dev server for HMR, per §8.2's diagram.
It is a process to supervise rather than a protocol to understand, and none of
the risk this phase carried applies to it.

**Rust-side restart-on-change.** A plain rebuild-and-restart; the spec already
notes that state lives in Rust, so it is a real restart with nothing subtle in
it.

**Reload on Windows and Linux.** `duet-dev` itself is platform-independent —
a child process, a TCP socket and the filesystem — and only
`locate::engine_switches` is documented against a specific embedder. Untested
anywhere but macOS, and not claimed.

## Done criteria

- [x] `frontend_server` client: spawn, drive, parse; survives death, hangs,
      stderr and a changed protocol
- [x] VM-service client over WebSocket; never `force: true`
- [x] debounced file watcher
- [x] the fd-1 redirect replaced, with the cost of each route stated
- [x] state proven to survive a reload, against a real engine
- [x] no `unwrap`/`expect`/`panic!` in non-test code
- [x] every failure names its stage; nothing waits without a deadline
- [x] workspace coverage gate held (95.5 %, floor 90 %)
