/**
 * The `wry` adapter: correlation over a one-way IPC channel, and the object →
 * text bridge.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import {
  decodeValueText,
  DuetTransportError,
  duetInt,
  duetValueEquals,
  type DuetValue,
} from '../src/index.ts';
import {
  connectWryDuet,
  stringifyHostMessage,
  UNCORRELATED_ID,
  WryTransport,
  type WryWindow,
} from '../src/wry.ts';

/** A stand-in for the page `wry` injects into. */
function fakeWindow(): WryWindow & { posted: string[] } {
  const posted: string[] = [];
  return { posted, ipc: { postMessage: (message) => posted.push(message) } };
}

describe('correlation', () => {
  // `timeout` is the assertion, not decoration: `node --test` applies no
  // per-test deadline of its own, so a client that never settles this promise
  // would WEDGE the whole run rather than fail one test. Measured — the
  // pre-fix bootstrap hung the suite until it was killed by hand.
  const HANG_TIMEOUT_MS = 5_000;

  test('a reply is routed to the call that is waiting for it', async () => {
    const host = fakeWindow();
    const transport = new WryTransport(host);

    const first = transport.send('{"id":"1","kind":"get","path":"a"}');
    const second = transport.send('{"id":"2","kind":"get","path":"b"}');
    assert.deepStrictEqual(host.posted, [
      '{"id":"1","kind":"get","path":"a"}',
      '{"id":"2","kind":"get","path":"b"}',
    ]);

    // Out of order, as a host is free to answer.
    host.__duet?.onResponse({
      id: '2',
      kind: 'value',
      value: { t: 'i', v: '2' },
    });
    host.__duet?.onResponse({
      id: '1',
      kind: 'value',
      value: { t: 'i', v: '1' },
    });

    assert.match((await second) as string, /"v":"2"/);
    assert.match((await first) as string, /"v":"1"/);
  });

  test('a reply for an unknown id is dropped rather than mis-routed', async () => {
    const host = fakeWindow();
    const transport = new WryTransport(host);
    const pending = transport.send('{"id":"1","kind":"get","path":"a"}');

    // A reply to a request this client never sent — a second guest sharing the
    // page, or a host bug. There is no caller to hand it to.
    assert.doesNotThrow(() => host.__duet?.onResponse({ id: '9', kind: 'done' }));
    assert.doesNotThrow(() => host.__duet?.onResponse('not an object'));
    assert.doesNotThrow(() => host.__duet?.onResponse(null));

    host.__duet?.onResponse({ id: '1', kind: 'done' });
    assert.match((await pending) as string, /"kind":"done"/);
  });

  test(
    'a reply the host could not correlate settles the call instead of hanging',
    { timeout: HANG_TIMEOUT_MS },
    async () => {
      // THE regression test. `{"kind":"failed","id":"0"}` is the host saying it
      // could not read the id of the request it is refusing — a lone UTF-16
      // surrogate does it, and so does nesting past the wire's limit. This map is
      // keyed by the id the request carried, so "0" matches nothing; the previous
      // behaviour dropped the reply there and left this promise unsettled
      // forever, with no error and no timeout.
      const host = fakeWindow();
      const transport = new WryTransport(host);
      const pending = transport.send('{"id":"1","kind":"get","path":"a"}');

      host.__duet?.onResponse({
        kind: 'failed',
        id: UNCORRELATED_ID,
        message: 'malformed JSON: lone leading surrogate in hex escape',
      });

      await assert.rejects(pending, (error: unknown) => {
        assert.ok(error instanceof DuetTransportError, 'must be a transport error');
        // The host's own account of what went wrong has to survive: it is the
        // only explanation of *why*, and a bare correlation complaint is the
        // wrong half of the story.
        assert.match(error.message, /surrogate/);
        return true;
      });
    },
  );

  test(
    'every outstanding call is settled, not just the first',
    { timeout: HANG_TIMEOUT_MS },
    async () => {
      // Neither the host nor this transport can say WHICH request the host failed
      // to read. Rejecting the set that contains it is the only sound superset:
      // a spurious rejection is visible and retryable, a hang is neither.
      const host = fakeWindow();
      const transport = new WryTransport(host);
      const first = transport.send('{"id":"1","kind":"get","path":"a"}');
      const second = transport.send('{"id":"2","kind":"get","path":"b"}');

      host.__duet?.onResponse({
        kind: 'failed',
        id: UNCORRELATED_ID,
        message: 'nope',
      });

      await assert.rejects(first, DuetTransportError);
      await assert.rejects(second, DuetTransportError);
    },
  );

  test('an unmatched id that is NOT the sentinel still leaves pending calls alone', async () => {
    // A reply to a request this client never sent — a second guest sharing the
    // page, or a stale reply after a reload. Dropping that one is correct;
    // failing every in-flight call because of it would not be.
    const host = fakeWindow();
    const transport = new WryTransport(host);
    const pending = transport.send('{"id":"1","kind":"get","path":"a"}');

    host.__duet?.onResponse({
      kind: 'failed',
      id: '9',
      message: 'someone else',
    });
    host.__duet?.onResponse({
      kind: 'value',
      id: UNCORRELATED_ID,
      value: null,
    });

    host.__duet?.onResponse({ id: '1', kind: 'done' });
    assert.match((await pending) as string, /"kind":"done"/);
  });

  test('a request with no usable id is refused rather than hanging forever', () => {
    const transport = new WryTransport(fakeWindow());
    assert.throws(() => transport.send('not json'), DuetTransportError);
    assert.throws(() => transport.send('{"kind":"done"}'), DuetTransportError);
    assert.throws(() => transport.send('{"id":1,"kind":"done"}'), DuetTransportError);
  });

  test('no injected ipc resolves with null, the transport contract for "no host"', async () => {
    const transport = new WryTransport({});
    assert.equal(await transport.send('{"id":"1","kind":"get","path":"a"}'), null);
  });
});

describe('the object-to-text bridge', () => {
  test('a parsed object and its text are equivalent inputs', () => {
    assert.equal(stringifyHostMessage('{"a":1}'), '{"a":1}');
    assert.equal(stringifyHostMessage({ a: 1 }), '{"a":1}');
  });

  test('a negative-zero float payload keeps its sign across the bridge', () => {
    // JSON.stringify(-0) is "0", so a naive re-serialisation would drop the
    // sign with no error at all. The replacer maps -0 to the wire's own string
    // sentinel, which the value decoder reads straight back as -0.
    assert.equal(JSON.stringify({ t: 'f', v: -0 }), '{"t":"f","v":0}');
    const text = stringifyHostMessage({ t: 'f', v: -0 });
    assert.equal(text, '{"t":"f","v":"-0"}');
    const view = new DataView(new ArrayBuffer(8));
    view.setFloat64(0, (decodeValueText(text) as { value: number }).value);
    assert.equal(view.getBigUint64(0).toString(16).padStart(16, '0'), '8000000000000000');
  });

  test('a positive zero is left alone', () => {
    assert.equal(stringifyHostMessage({ t: 'f', v: 0 }), '{"t":"f","v":0}');
  });
});

describe('the installed bridge', () => {
  test('constructing the transport installs the hooks the host calls', () => {
    const host = fakeWindow();
    // The names are a contract with crates/duet-webview/src/lib.rs, whose
    // scripts are `window.__duet && window.__duet.onResponse(...)`. A rename on
    // either side drops every reply silently.
    // Checked on a throwaway page rather than on `host`: `assert` from
    // `node:assert/strict` is typed as an assertion function, so asserting
    // `host.__duet === undefined` would narrow it to `undefined` for the rest
    // of the test and the lines below would stop type-checking.
    assert.equal(typeof fakeWindow().__duet, 'undefined', 'a bare page has no bridge');
    new WryTransport(host);
    assert.equal(typeof host.__duet?.onResponse, 'function');
    assert.equal(typeof host.__duet?.onPush, 'function');
  });

  test('connectWryDuet returns a started client that speaks the protocol', async () => {
    const host = fakeWindow();
    const duet = connectWryDuet(host);

    const pending = duet.get('editor.zoom');
    assert.deepStrictEqual(host.posted, ['{"id":"1","kind":"get","path":"editor.zoom"}']);
    host.__duet?.onResponse({
      id: '1',
      kind: 'value',
      value: { t: 'i', v: '42' },
    });
    assert.ok(duetValueEquals((await pending) as DuetValue, duetInt(42n)));

    // And pushes reach the client without a separate `start()` call.
    let seen = 0;
    duet.onPush = () => seen++;
    host.__duet?.onPush({
      kind: 'notification',
      notification: {
        patch: { path: 'editor.zoom', value: { t: 'f', v: 1.5 } },
        subscriber: '1',
        subscription: '2',
      },
    });
    assert.equal(seen, 1);
  });

  test('a malformed push from the host is dropped, not thrown', () => {
    const host = fakeWindow();
    const duet = connectWryDuet(host);
    duet.onPush = () => {
      throw new Error('must not be reached');
    };
    for (const bad of [null, 42, { kind: 'nope' }, 'not json']) {
      assert.doesNotThrow(() => host.__duet?.onPush(bad));
    }
  });
});
