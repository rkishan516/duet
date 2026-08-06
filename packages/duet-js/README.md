# duet-protocol

The Duet wire protocol in **TypeScript**. Zero runtime dependencies — no DOM, no
Node built-ins, nothing from npm.

Duet is a Rust host that owns shared application state, with guests — a Flutter
engine, a `wry` webview — reading and writing that state over a tagged-JSON
protocol. This package is the JavaScript half of that protocol.

> **On the package name.** The directory is `packages/duet-js`, but `duet-js` is
> taken on npm (as is `duet`), so the published name is **`duet-protocol`** —
> which was free, and which names the Rust crate this package mirrors most
> closely (`crates/duet-protocol`).

## What is in the box

| Layer | Exports | Rust peer |
|---|---|---|
| Values | `DuetValue`, `duetInt`, `encodeValue`, `decodeValue` … | `duet_core::Value`, `duet-codec` |
| Paths | `DuetPath`, `parseDuetPath`, `formatDuetPath` | `duet_core::Path` |
| Envelope | `DuetRequest`, `DuetResponse`, `DuetPush` and their codecs | `duet-protocol` |
| Client | `DuetClient`, `DuetTransport` | `window.__duet` in `duet-webview` |
| Webview | `WryTransport`, `connectWryDuet` (from `duet-protocol/wry`) | `crates/duet-webview` |
| Typed runtime | `DuetField`, `DuetRouter`, `DuetCodec` … (from `duet-protocol/typed`) | (hand-written; Phase 4 generates against it) |

## Using it

```ts
import { DuetClient, duetFloat } from 'duet-protocol';
import { connectWryDuet } from 'duet-protocol/wry';

const duet = connectWryDuet(); // installs window.__duet, starts listening

await duet.set('editor.zoom', duetFloat(1.5));
const zoom = await duet.get('editor.zoom');

duet.onPush = (note) => console.info(note.path, note.value);
const sub = await duet.subscribe('editor');
await duet.unsubscribe(sub.id);
```

Outside a webview, supply your own transport — two members, and the whole seam
between this package and the outside world:

```ts
interface DuetTransport {
  send(request: string): Promise<string | null>;
  onPush: ((message: string) => void) | null;
}

const duet = new DuetClient(myTransport);
duet.start();
```

## Four things a JavaScript client must not get wrong

Every one of these is a place where the obvious code is silently wrong, and none
of them is caught by a round-trip test written against this client alone. They
are the reason the cross-language corpus exists.

### 1. `JSON.parse` has no depth limit

`serde_json` refuses input nested past 128 containers. V8 does not refuse at all
— it parsed a 100 000-deep document without complaint in this project's own
measurement. So a naive client **accepts** messages every Rust peer rejects, and
then feeds the result to a recursive decoder, which overflows the stack for
real. `decodeDuetJson` therefore runs an explicit depth check at
`MAX_JSON_DEPTH`, and that check is **iterative** — a recursive one dies on
exactly the input it exists to reject.

### 2. Integers past 2^53 need `bigint`

The wire's integer domain is `i64`. `DuetInt.value` is a `bigint`, and the
corpus pins `9007199254740993` — one above 2^53, where `Number` starts skipping
odd integers. Wire ids are `bigint` for the same reason; `i64::MAX` appears in
the corpus as an accepted id.

### 3. `JSON.stringify(-0)` is `"0"`

There is no way to write negative zero as a JSON number from JavaScript. It
therefore travels as the string sentinel `"-0"`, joining `"NaN"`, `"Infinity"`
and `"-Infinity"` — the four `f64` values with no portable JSON-number spelling.
Detection is `Object.is(n, -0)`; `n === -0` is also true for `+0` and would tag
every zero as negative.

### 4. Canonical key order, twice over

The host serializes through `serde_json::Map`, a `BTreeMap`, so object keys come
out in **code-point order** (equivalently UTF-8 byte order). Two separate
JavaScript problems stand in the way:

- The default `sort` and `<` compare UTF-16 **code units**, which disagree with
  code points above the BMP: `U+1F600` is the surrogate pair `D83D DE00`, and
  `0xD83D < 0xE000`, so the built-ins sort it *before* `U+E000` where Rust sorts
  it *after*. `compareDuetMapKeys` is the correct comparator.
- Even with a correct comparator, `JSON.stringify` emits an object's
  **integer-like** keys (`"0"`, `"1"`, `"42"`) first and in ascending numeric
  order, whatever order they were inserted in. No comparator overrides it,
  because no comparator ever runs — `JSON.stringify({'0':1,'!':2})` is
  `{"0":1,"!":2}` where canonical order is `"!"` then `"0"`.

So this package does not hand objects to `JSON.stringify` at all. Map values are
`Map`s (which preserve insertion order for every key), and `encodeDuetJson`
writes the JSON text itself, sorting as it goes. Strings still go through
`JSON.stringify`, whose escaper already agrees with `serde_json` byte for byte.

`crates/duet-webview/src/bootstrap.rs` documents the integer-like-key problem as
a caveat its guests must live with. This package does not have that caveat, and
`test/json.test.ts` pins the difference.

## The typed runtime

A second, optional entry point — `duet-protocol/typed` — turns the untyped value
tree into typed fields with a local mirror that stays correct while another guest
writes the same store:

```ts
import { DuetClient } from 'duet-protocol';
import { DuetField, DuetRouter, duetFloatCodec } from 'duet-protocol/typed';

const router = new DuetRouter(new DuetClient(transport));
router.attach();

const zoom = new DuetField(router, 'editor.zoom', duetFloatCodec);
const watch = await zoom.watch((reading) => {
  switch (reading.kind) {
    case 'present':  repaint(reading.value); break;
    case 'none':     break;  // the path holds Value::Null
    case 'absent':   break;  // there is no node at the path
    case 'mismatch': report(reading.found); break;
  }
});
```

Four things it is worth knowing before reading the code:

**A type mismatch is a reading, not an exception.** Another guest can write any
value to any path — the repository's two-guest proof has a webview and a Flutter
engine writing one store simultaneously — so a typed watcher *will* meet a value
its codec refuses. It arrives through a push, where there is no call stack to
throw into, so `get` reports it the same way `watch` does.

**`None` and "no such path" stay apart.** Rust's `Option<T>` lowers `None` to
`Value::Null`, which is a value that exists. A path with no node at all is a
different thing, and `DuetOptionalField` reports `'none'` for the first and
`'absent'` for the second. Measured on the host, with `Option<Editor> = None`, a
child path behaves three different ways at once: `get` answers null, `subscribe`
succeeds, and `set` **fails**. The typed layer surfaces all three rather than
papering over them.

**`DuetCodec<T extends {}>`'s bound is not stylistic.** `decode` answers `null`
for "refused"; a nullable `T` would make that indistinguishable from "decoded,
and the answer is null" — collapsing the very distinction the layer above is
built on. `test/typed/codec.test.ts` pins it with `@ts-expect-error`, which fails
the build in *both* directions: if the nullable codec ever compiles, and if the
directive is ever left dangling.

**One router owns the push slot.** `DuetClient.onPush` is a single mutable slot,
so a second owner silently steals the first one's notifications, and the symptom
is a watcher that just stops updating. `DuetRouter.attach` refuses to install
itself over an existing owner.

## Conformance

`test/wire-corpus.test.ts` runs this package against `corpus/wire-corpus.json`
at the repository root — the golden wire corpus generated by the Rust workspace
and consumed as a peer by every implementation of the format.

All **50 accept** cases and all **20 reject** cases are consumed. Accept cases
are decoded, compared against a witness built **from the decoded value** in a
deliberately different representation (floats as IEEE-754 hex bits, strings as
UTF-8 byte arrays, maps as ordered entry arrays), and re-encoded — byte-exactly
where the corpus says so, structurally otherwise. Reject cases must throw, and
must throw `DuetCodecError` specifically, carrying the recorded reason code. The
case totals are pinned as literals and the number of cases that actually ran is
checked at the end, so a truncated or silently skipped corpus fails.

`test/bootstrap-parity.test.ts` additionally loads the hand-written guest script
out of `crates/duet-webview/src/bootstrap.rs`, runs it in a `node:vm` sandbox,
and checks it against the same corpus for the subset it can reach — the float
sentinels, the key comparator, canonical ids, and the response hook. Two
JavaScript implementations of one format drift; that file is what stops it
silently.

## Development

No test framework and no bundler. Sources import each other with explicit `.ts`
specifiers, so Node's built-in type stripping runs them directly:

```sh
npm install
npm run build      # tsc → dist/, with .d.ts and source maps
npm test           # typecheck (src + test), then node --test
```

`erasableSyntaxOnly` is on, which bans the TypeScript constructs Node's type
stripping cannot run (`enum`, `namespace`, parameter properties). Without it the
build would pass and `npm test` would fail with a confusing syntax error.

## License

MIT OR Apache-2.0, at your option.
