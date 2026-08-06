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

const corpus = JSON.parse(readFileSync(CORPUS_URL, 'utf8')) as { accept: AcceptCase[] };

/** The bootstrap's guest API, as far as this file drives it. */
interface BootstrapDuet {
  get(path: string): Promise<unknown>;
  float(n: number): { t: 'f'; v: number | string };
  toFloat(value: { v: unknown }): number;
  map(entries: Record<string, unknown>): { t: 'm'; v: Record<string, unknown> };
  compareKeys(a: string, b: string): number;
  onResponse(response: { id: string }): void;
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
    window: { ipc: { postMessage: (message: string) => posted.push(message) } } as {
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
    for (const member of ['get', 'float', 'toFloat', 'map', 'compareKeys', 'onResponse', 'onPush']) {
      assert.equal(
        typeof (duet as unknown as Record<string, unknown>)[member],
        'function',
        `the bootstrap must expose ${member}`,
      );
    }
  });
});

describe('float sentinels agree with this package, case by case', () => {
  const floatCases = corpus.accept.filter((c) => c.layer === 'value' && c.name.startsWith('value/float/'));

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
      encodeValueText(duetMap(new Map([['!', duetNull()], ['0', duetNull()]]))),
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
    assert.equal(formatDuetPath((request as { path: import('../src/index.ts').DuetPath }).path), 'editor.zoom');
  });
});

describe('ids and the response hook', () => {
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
});
