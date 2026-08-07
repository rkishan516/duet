/**
 * Differential conformance between this package and the webview bootstrap.
 *
 * # Why this file exists
 *
 * There are two JavaScript implementations of parts of the Duet wire format in
 * this repository:
 *
 * - this package, the full typed client a real app installs from npm; and
 * - `BOOTSTRAP_HTML` in crates/duet-webview/src/bootstrap.rs, a ~130-line
 *   hand-written script embedded in a Rust string constant, which is the
 *   default page a `wry` webview boots with and the guest the macOS examples
 *   drive.
 *
 * Keeping the bootstrap hand-written (rather than making it a committed build
 * of this package) is a deliberate choice — see this package's README for the
 * reasoning. The cost of that choice is **drift**: two implementations of the
 * same rules, each tested against its own idea of what is correct. The Rust
 * tests next to `BOOTSTRAP_HTML` can only assert that certain substrings are
 * present, because a Rust test cannot execute JavaScript.
 *
 * This file pays that cost down. It loads the *actual* script out of
 * `bootstrap.rs`, runs it in a `node:vm` sandbox with a stubbed `window`, and
 * checks it against the same golden corpus this package is checked against, for
 * the subset the bootstrap can reach: the float sentinels, the code-point map
 * key comparator, canonical request ids, and the response hook. A change to
 * either implementation that breaks the agreement fails here.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, test } from 'node:test';
import vm from 'node:vm';

import {
  compareDuetMapKeys,
  decodeRequestText,
  decodeValueText,
  duetInt,
  duetMap,
  duetNull,
  encodeValue,
  encodeValueText,
  formatDuetPath,
  type CanonicalJson,
} from '../src/index.ts';

const BOOTSTRAP_RS = new URL('../../../crates/duet-webview/src/bootstrap.rs', import.meta.url);
const CORPUS_URL = new URL('../../../corpus/wire-corpus.json', import.meta.url);

interface AcceptCase {
  readonly layer: string;
  readonly name: string;
  readonly wire: string;
  readonly witness: { readonly k: string; readonly bits?: string };
}

const corpus = JSON.parse(readFileSync(CORPUS_URL, 'utf8')) as {
  accept: AcceptCase[];
};

/** The bootstrap's guest API, as far as this file drives it. */
interface BootstrapDuet {
  get(path: string): Promise<unknown>;
  invoke(command: string, args?: Record<string, unknown>): Promise<unknown>;
  float(n: number): { t: 'f'; v: number | string };
  toFloat(value: { v: unknown }): number;
  map(entries: Record<string, unknown>): { t: 'm'; v: Record<string, unknown> };
  compareKeys(a: string, b: string): number;
  onResponse(response: { id: string; kind?: string; message?: string }): void;
  onPush(push: unknown): void;
  pushes: unknown[];
}

/**
 * Extracts the `<script>` body out of `BOOTSTRAP_HTML`.
 *
 * Reading the Rust source rather than a copy is the entire point: a copy would
 * be one more thing to drift.
 */
function readBootstrapScript(): string {
  const source = readFileSync(BOOTSTRAP_RS, 'utf8');
  const open = source.indexOf('<script>');
  const close = source.indexOf('</script>');
  assert.ok(open >= 0 && close > open, 'bootstrap.rs must still contain a <script> block');
  return source.slice(open + '<script>'.length, close);
}

/** Boots the bootstrap script in a sandbox and returns its `window.__duet`. */
function bootBootstrap(): { duet: BootstrapDuet; posted: string[] } {
  const posted: string[] = [];
  const element = { textContent: '' };
  const sandbox = {
    window: {
      ipc: { postMessage: (message: string) => posted.push(message) },
    } as {
      ipc: { postMessage(message: string): void };
      __duet?: BootstrapDuet;
    },
    document: { getElementById: () => element },
  };
  vm.createContext(sandbox);
  vm.runInContext(readBootstrapScript(), sandbox, { filename: 'bootstrap.js' });
  assert.ok(sandbox.window.__duet, 'the bootstrap must install window.__duet');
  return { duet: sandbox.window.__duet, posted };
}

/** The IEEE-754 bits of `v` as 16 lowercase hex characters. */
function bits(v: number): string {
  const view = new DataView(new ArrayBuffer(8));
  view.setFloat64(0, v);
  return view.getBigUint64(0).toString(16).padStart(16, '0');
}

/** The `"v"` payload of a tagged value this package encoded. */
function payloadOf(wire: string): unknown {
  return (encodeValue(decodeValueText(wire)) as ReadonlyMap<string, CanonicalJson>).get('v');
}

describe('the bootstrap loads and installs its hooks', () => {
  test('the script still runs and defines the guest API', () => {
    const { duet } = bootBootstrap();
    for (const member of [
      'get',
      'invoke',
      'float',
      'toFloat',
      'map',
      'compareKeys',
      'onResponse',
      'onPush',
    ]) {
      assert.equal(
        typeof (duet as unknown as Record<string, unknown>)[member],
        'function',
        `the bootstrap must expose ${member}`,
      );
    }
  });
});

describe('the bootstrap can invoke a command', () => {
  // The fixture page is the only guest a macOS example can drive over a real
  // transport, so an `invoke` it cannot send is command RPC that has never run
  // over anything but stdio. A Rust test cannot execute this script; this is
  // where what it actually posts on the wire is read.

  test('an invoke posts the request kind and command the host decodes', () => {
    const { duet, posted } = bootBootstrap();
    void duet.invoke('subtract', { a: { t: 'i', v: '10' }, b: { t: 'i', v: '3' } });
    assert.equal(posted.length, 1, 'exactly one message must be posted');
    const sent = JSON.parse(posted[0] as string) as Record<string, unknown>;
    assert.equal(sent['kind'], 'invoke');
    assert.equal(sent['command'], 'subtract');
    assert.equal(sent['id'], '1', 'ids are canonical decimal strings from 1');
    assert.deepEqual(sent['args'], {
      t: 'm',
      v: { a: { t: 'i', v: '10' }, b: { t: 'i', v: '3' } },
    });
  });

  test('the arguments travel as a tagged map with its keys in code-point order', () => {
    // The reason `invoke` wraps `args` with `map()` rather than passing the
    // object through: `args` is a `Value::Map` on the wire, and the wire orders
    // a map's keys by code point. Built out of order deliberately, and across
    // the surrogate boundary, where a default JavaScript sort disagrees with
    // Rust.
    const { duet, posted } = bootBootstrap();
    void duet.invoke('save', {
      '\u{1F600}': { t: 'n' },
      '\u{FFFD}': { t: 'n' },
      '\u{E000}': { t: 'n' },
    });
    const sent = posted[0] as string;
    const args = (JSON.parse(sent) as { args: { v: Record<string, unknown> } }).args;
    assert.deepEqual(Object.keys(args.v), ['\u{E000}', '\u{FFFD}', '\u{1F600}']);
  });

  test('a command with no arguments still sends an empty tagged map', () => {
    // `args` is required by `duet_protocol::decode_request`; omitting it would
    // be refused, and sending `{}` untagged would be refused too.
    const { duet, posted } = bootBootstrap();
    void duet.invoke('session.ping');
    const sent = JSON.parse(posted[0] as string) as Record<string, unknown>;
    assert.deepEqual(sent['args'], { t: 'm', v: {} });
  });

  test('a returned reply settles the call it answers', () => {
    // The correlation half. A reply this map never matched would leave the
    // promise unsettled forever, which is the failure shape this project has
    // found twice.
    const { duet, posted } = bootBootstrap();
    const call = duet.invoke('subtract', { a: { t: 'i', v: '10' }, b: { t: 'i', v: '3' } });
    const id = (JSON.parse(posted[0] as string) as { id: string }).id;
    duet.onResponse({ id, kind: 'returned' });
    return call.then((response) => {
      assert.deepEqual(response, { id, kind: 'returned' });
    });
  });
});

describe('float sentinels agree with this package, case by case', () => {
  const floatCases = corpus.accept.filter(
    (c) => c.layer === 'value' && c.name.startsWith('value/float/'),
  );

  test('the corpus has float cases to check', () => {
    // Guards the failure where a filter silently matches nothing and this whole
    // suite becomes a no-op.
    assert.ok(floatCases.length >= 10, `only ${String(floatCases.length)} float cases found`);
  });

  for (const c of floatCases) {
    test(c.name, () => {
      const { duet } = bootBootstrap();

      // Decode: the bootstrap's `toFloat` must read the corpus payload to
      // exactly the double the corpus witness pins, bit for bit.
      const wirePayload = JSON.parse(c.wire) as { v: unknown };
      assert.equal(
        bits(duet.toFloat({ v: wirePayload.v })),
        c.witness.bits,
        `${c.name}: the bootstrap decoded a different double`,
      );

      // Encode: given that same double, the bootstrap must produce the payload
      // this package produces. This is the assertion that would have caught
      // `-0` losing its sign.
      const value = duet.toFloat({ v: wirePayload.v });
      assert.deepStrictEqual(
        duet.float(value).v,
        payloadOf(c.wire),
        `${c.name}: the bootstrap and this package disagree on the payload`,
      );
    });
  }

  test('negative zero survives both halves, which is the whole reason for the sentinel', () => {
    const { duet } = bootBootstrap();
    assert.equal(duet.float(-0).v, '-0');
    assert.equal(duet.float(0).v, 0, '+0 must NOT be tagged; Object.is is what separates them');
    assert.equal(bits(duet.toFloat({ v: '-0' })), '8000000000000000');
  });
});

describe('map key order agrees with this package', () => {
  const mapCase = corpus.accept.find((c) => c.name === 'value/map/code_point_order');

  test('the comparator matches compareDuetMapKeys over the surrogate boundary', () => {
    const { duet } = bootBootstrap();
    const keys = ['\u{1F600}', '', '�', '', 'a', 'ab', 'é'];
    for (const a of keys) {
      for (const b of keys) {
        assert.equal(
          Math.sign(duet.compareKeys(a, b)),
          Math.sign(compareDuetMapKeys(a, b)),
          `${JSON.stringify(a)} vs ${JSON.stringify(b)}`,
        );
      }
    }
  });

  test('__duet.map reproduces the corpus bytes for value/map/code_point_order', () => {
    assert.ok(mapCase, 'the corpus must still carry value/map/code_point_order');
    const { duet } = bootBootstrap();
    const built = duet.map({
      '\u{1F600}': { t: 'i', v: '3' },
      '': { t: 'i', v: '1' },
      '�': { t: 'i', v: '2' },
    });
    assert.equal(JSON.stringify(built), mapCase.wire, 'the bootstrap emitted a different order');
    // And this package agrees, from the other direction.
    assert.equal(
      encodeValueText(
        duetMap(
          new Map([
            ['\u{1F600}', duetInt(3n)],
            ['', duetInt(1n)],
            ['�', duetInt(2n)],
          ]),
        ),
      ),
      mapCase.wire,
    );
  });
});

describe('the documented divergences, pinned so they stay documented', () => {
  test('the bootstrap cannot emit integer-like map keys canonically; this package can', () => {
    // `bootstrap.rs` documents this as a caveat no JS guest working through a
    // plain object can code around: ECMAScript puts integer-like keys first, in
    // ascending numeric order, whatever a sort did. It is pinned here rather
    // than left as a comment, because a caveat nobody tests is a caveat nobody
    // notices becoming false — or becoming worse.
    //
    // No golden-corpus case has integer-like map keys, so nothing fails over
    // it today. A value captured from the bootstrap must not be used as a
    // byte-exact fixture if its keys look like array indices.
    const { duet } = bootBootstrap();
    const built = duet.map({ '!': { t: 'n' }, '0': { t: 'n' } });
    assert.equal(
      JSON.stringify(built),
      '{"t":"m","v":{"0":{"t":"n"},"!":{"t":"n"}}}',
      'the bootstrap still has the integer-like-key limitation',
    );
    assert.equal(
      encodeValueText(
        duetMap(
          new Map([
            ['!', duetNull()],
            ['0', duetNull()],
          ]),
        ),
      ),
      '{"t":"m","v":{"!":{"t":"n"},"0":{"t":"n"}}}',
      'this package does not, because it writes the JSON text itself',
    );
  });

  test('the bootstrap emits valid but non-canonical envelope key order', () => {
    // `Object.assign({kind, id}, extra)` produces declaration order, not the
    // byte order Rust's BTreeMap emits. That is *decodable* — decoding is
    // order-insensitive — so the webview guest works, and the examples pass.
    // It is still a divergence from this package, whose bytes are canonical, so
    // it is stated here rather than discovered later by someone diffing two
    // guests' traffic.
    const { duet, posted } = bootBootstrap();
    void duet.get('editor.zoom');
    assert.deepStrictEqual(posted, ['{"kind":"get","id":"1","path":"editor.zoom"}']);
    assert.notEqual(posted[0], '{"id":"1","kind":"get","path":"editor.zoom"}');

    // But it decodes under this package's decoder, which is the property that
    // actually matters for the two guests to interoperate.
    const request = decodeRequestText(posted[0] as string);
    assert.equal(request.kind, 'get');
    assert.equal(request.id, 1n);
    assert.equal(
      formatDuetPath((request as { path: import('../src/index.ts').DuetPath }).path),
      'editor.zoom',
    );
  });
});

describe('ids and the response hook', () => {
  // `timeout` is the assertion, not decoration: `node --test` applies no
  // per-test deadline of its own, so a client that never settles this promise
  // would WEDGE the whole run rather than fail one test. Measured — the
  // pre-fix bootstrap hung the suite until it was killed by hand.
  const HANG_TIMEOUT_MS = 5_000;

  test('ids are canonical decimal strings, so the host can echo them back', () => {
    // A guest that sent "007" would be answered "7", never match its own
    // pending entry, and hang forever with no error at all.
    const { duet, posted } = bootBootstrap();
    void duet.get('a');
    void duet.get('b');
    const ids = posted.map((text) => (JSON.parse(text) as { id: string }).id);
    assert.deepStrictEqual(ids, ['1', '2']);
    for (const id of ids) {
      assert.doesNotThrow(() => decodeRequestText(`{"id":"${id}","kind":"get","path":"a"}`));
    }
  });

  test('onResponse resolves the call that is waiting for it', async () => {
    const { duet, posted } = bootBootstrap();
    const pending = duet.get('a');
    const id = (JSON.parse(posted[0] as string) as { id: string }).id;
    duet.onResponse({ id, kind: 'done' } as { id: string });
    assert.deepStrictEqual(await pending, { id, kind: 'done' });
  });

  test('onPush collects host pushes', () => {
    const { duet } = bootBootstrap();
    duet.onPush({ kind: 'notification' });
    assert.equal(duet.pushes.length, 1);
  });

  test(
    'a reply the host could not correlate settles the call instead of hanging',
    { timeout: HANG_TIMEOUT_MS },
    async () => {
      // THE regression test, driven against the ACTUAL bootstrap script rather
      // than against a substring of it.
      //
      // `{"kind":"failed","id":"0"}` is the host saying it could not read the id
      // of the request it is refusing — `RequestId::UNCORRELATED` in
      // crates/duet-protocol/src/message.rs. A lone UTF-16 surrogate anywhere in
      // the message produces exactly this. The bootstrap's pending map is keyed by
      // the id it sent, so "0" matches nothing; before this fix the reply was
      // dropped there and the promise never settled — a hang with no error and no
      // timeout, the same failure shape this project has found twice before.
      const { duet, posted } = bootBootstrap();
      const pending = duet.get('a');
      assert.equal(posted.length, 1, 'the request must have gone out');

      duet.onResponse({
        kind: 'failed',
        id: '0',
        message: 'malformed JSON: lone leading surrogate in hex escape',
      });

      //
      // `instanceof Error` would be wrong here: the script runs in a `vm` context
      // with its own realm, so the `Error` it constructs is not this realm's
      // `Error` and the check fails for a reason that has nothing to do with the
      // behaviour under test. The message is what matters anyway — the host's
      // account of what went wrong has to survive, since it is the only
      // explanation of *why*.
      await assert.rejects(pending, (error: unknown) => {
        const message = String((error as { message?: unknown }).message);
        assert.match(message, /surrogate/);
        assert.match(message, /could not correlate/);
        return true;
      });
    },
  );

  test(
    'every outstanding call is settled, not just the first',
    { timeout: HANG_TIMEOUT_MS },
    async () => {
      // Neither the host nor the guest can say WHICH request the host failed to
      // read, so rejecting the set that contains it is the only sound superset.
      // A spurious rejection is visible and retryable; a hang is neither.
      const { duet } = bootBootstrap();
      const first = duet.get('a');
      const second = duet.get('b');
      duet.onResponse({ kind: 'failed', id: '0', message: 'nope' });
      await assert.rejects(first);
      await assert.rejects(second);
    },
  );

  test('an unmatched id that is NOT the sentinel leaves pending calls alone', async () => {
    // A reply to a request this guest never sent — a second guest sharing the
    // page, or a stale reply after a reload. Dropping that one is correct.
    const { duet, posted } = bootBootstrap();
    const pending = duet.get('a');
    duet.onResponse({ kind: 'failed', id: '9', message: 'someone else' });
    const id = (JSON.parse(posted[0] as string) as { id: string }).id;
    duet.onResponse({ id, kind: 'done' });
    assert.deepStrictEqual(await pending, { id, kind: 'done' });
  });
});
