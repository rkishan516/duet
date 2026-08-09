# 09 — Known limitations

What Duet does not do yet, and what has not been proven. Each entry says how it
was found and what it would take to close.

Nothing here is speculative: every gap below was hit by building
[`examples/showcase/`](../examples/showcase/), the first program written *as a
user* rather than as a proof harness. That is what a showcase is for.

---

## 1. A webview surface cannot be given a page

`WebviewSurface::new` and `with_commands` both hard-code the built-in bootstrap
page ([`webview.rs:154`](../crates/duet-backend-macos/src/webview.rs)). There is
no URL, HTML, custom-protocol or init-script parameter.

The showcase works around it by bundling its guest to one file and evaluating it
into the page, letting the script rebuild the DOM. That is a legitimate use of
`WryTransport`, but it is not how an application ships a UI.

**To close:** a builder taking a page source (URL, HTML, or a custom protocol
handler), with the bootstrap remaining the default.

## 2. An embedder must re-emit the Flutter rpath

`cargo:rustc-link-arg` from `duet-backend-macos`'s build script does not
propagate to a downstream binary, so a crate depending on it fails at launch
with `Library not loaded: @rpath/FlutterMacOS.framework/…`. The showcase carries
its own `build.rs` solely to re-emit it.

**To close:** `links = "FlutterMacOS"` plus build metadata, so the framework
directory reaches dependants through Cargo rather than by copy-paste.

## 3. Codegen emits no Rust accessors

`duet generate` emits Dart and TypeScript. The Rust host — the side that *owns*
the definition — still writes path literals through `TypedStore::field`.

So the one participant guaranteed to be in sync is the one with no generated
client, and a renamed field breaks the host silently while both guests are
updated for it. The showcase host writes `"document.lines"` by hand for exactly
this reason.

**To close:** a Rust emitter. The schema already carries everything needed; this
is additive.

## 4. `watch` does not deliver the current value to its callback

A watcher's callback fires on *changes*. The value at subscribe time arrives
separately as `DuetWatch.current`
([`duet_router.dart:139`](../packages/duet/lib/src/typed/duet_router.dart)).

This is documented and deliberate, but it is a sharp edge: it caught the
showcase once, where the guest that attached second never learned what its peer
had already written. Both guests now seed from `current` explicitly.

**To close:** either an opt-in "deliver current immediately" flag, or a combined
stream that emits the current value first.

---

## Platform coverage

**macOS only.** Linux and Windows backends are not written.

This is not an oversight and not a small remaining task. The largest unretired
risk in the project is Flutter's GTK embedder sharing a single main loop with
WebKitGTK — the Linux analogue of what
[Spike B](../docs/superpowers/spikes/2026-08-04-phase0-findings.md) proved for
`tao` + Flutter + `wry` on macOS. Nobody has run it.

The backends could be written blind. They would not be *proven*, and every other
claim in this repository is backed by something that was actually run. Writing
unverified platform code and describing it the same way would devalue the rest.

## What has never been seen on a display

Every visual claim in this repository is inferred from values read back out of
the store, never from something a person watched.

The development machine has no reachable on-screen WindowServer for spawned
processes. Windows are created and both renderers draw into them; nothing
appears on a monitor. The showcase, the six example programs and the hot-reload
proof all report by reading state back, which is why they can assert at all.

Specifically unproven:

| Claim | Status |
|---|---|
| Anything rendered correctly | Never observed |
| Real mouse or keyboard input to a `WKWebView` | Unproven — synthetic events reach a Flutter view but not a `WKWebView` ([Spike B](superpowers/spikes/2026-08-04-phase0-findings.md)) |
| Release/AOT Flutter builds | Only debug/JIT has ever been built; `duet dev` cannot work against AOT at all |
| CI on a real runner | The workflow is YAML-validated and its steps run locally; the ubuntu jobs have not executed |

## Deferred by choice

These are designed but not built, in rough priority order:

- **Type surface extensions** — `char`, tuples, fixed-size arrays, `BTreeSet`,
  and data-carrying enums. Each is additive to a proven pipeline.
- **Async command handlers.** Duet takes no async runtime dependency anywhere,
  and adding one would be the largest dependency decision the project has made.
  A `Responder` seam is sketched for it.
- **Cancellation and deadlines for commands.** A handler that never returns
  currently occupies its transport thread.
- **Per-subscription sequence numbers** on notifications.
- **Reactive adapters** in `duet_flutter`, so a watcher drives a widget directly.
