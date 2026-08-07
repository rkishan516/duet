/**
 * The guest client, mirroring `packages/duet/test/duet_client_test.dart`.
 *
 * @module
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';

import {
  decodeRequestText,
  DuetClient,
  DuetCodecError,
  DuetFailure,
  DuetTransportError,
  DUET_CHANNEL_NAME,
  DUET_ROOT_PATH,
  duetInt,
  duetMap,
  duetNull,
  duetStr,
  duetValueEquals,
  encodeResponseText,
  formatDuetPath,
  type DuetInvocation,
  type DuetInvokeRequest,
  type DuetNotification,
  type DuetRequest,
  type DuetResponse,
  type DuetTransport,
  type DuetValue,
} from '../src/index.ts';

/**
 * A {@link DuetTransport} driven by a function, standing in for a webview's IPC
 * channel.
 *
 * This is the whole reason the transport is an interface: the entire protocol
 * is exercised here against a dozen lines of fake, with no webview, no host
 * process and no build step.
 */
class FakeTransport implements DuetTransport {
  onPush: ((message: string) => void) | null = null;

  /** Every request text this transport was handed, in order. */
  readonly sent: string[] = [];

  readonly #reply: (request: string) => Promise<string | null>;

  constructor(reply: (request: string) => Promise<string | null>) {
    this.#reply = reply;
  }

  /** A transport with no host listening: `send` resolves with null. */
  static silent(): FakeTransport {
    return new FakeTransport(() => Promise.resolve(null));
  }

  send(request: string): Promise<string | null> {
    this.sent.push(request);
    return this.#reply(request);
  }

  /** True while a client is listening for pushes. */
  get isListening(): boolean {
    return this.onPush !== null;
  }

  /** Delivers an unsolicited message, as a host would. */
  deliverPush(message: string): void {
    this.onPush?.(message);
  }
}

/** Answers each request with the reply its `kind` calls for. */
function echoHost(request: string): Promise<string | null> {
  const req = decodeRequestText(request);
  const id = req.id.toString();
  switch (req.kind) {
    case 'set':
    case 'unsubscribe':
      return Promise.resolve(`{"kind":"done","id":"${id}"}`);
    case 'get':
      return Promise.resolve(`{"kind":"value","id":"${id}","value":{"t":"i","v":"42"}}`);
    case 'subscribe':
      return Promise.resolve(
        `{"kind":"subscribed","id":"${id}","subscription":"7","snapshot":{"t":"i","v":"42"}}`,
      );
    case 'invoke':
      return Promise.resolve(echoInvoke(req));
  }
}

/**
 * Answers an `invoke` all three ways the protocol allows, chosen by name.
 *
 * `subtract` computes `a - b` from the **decoded arguments** rather than
 * answering a constant, and subtraction is not commutative — so a client that
 * encoded its arguments under swapped names changes the answer here. A fake that
 * returned `5` whatever it was handed would pass against a completely broken
 * argument encoder.
 */
function echoInvoke(req: DuetInvokeRequest): string {
  switch (req.command) {
    case 'subtract': {
      const a = req.args.get('a');
      const b = req.args.get('b');
      if (a?.kind !== 'int' || b?.kind !== 'int') {
        return encodeResponseText({
          kind: 'failed',
          id: req.id,
          message: 'subtract expects int arguments "a" and "b"',
        });
      }
      return encodeResponseText({
        kind: 'returned',
        id: req.id,
        value: duetInt(a.value - b.value),
      });
    }
    case 'nothing':
      return encodeResponseText({ kind: 'returned', id: req.id, value: duetNull() });
    case 'raise':
      return encodeResponseText({
        kind: 'raised',
        id: req.id,
        error: duetMap([
          ['code', duetStr('insufficient_funds')],
          ['short_by', duetInt(250n)],
        ]),
      });
    // A host answering an `invoke` with a reply that answers no `invoke`.
    case 'confused':
      return encodeResponseText({ kind: 'done', id: req.id });
    default:
      return encodeResponseText({
        kind: 'failed',
        id: req.id,
        message: `no command named "${req.command}" is registered for this surface`,
      });
  }
}

describe('request/response', () => {
  test('get, set, subscribe and unsubscribe round-trip', async () => {
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);

    await duet.set('counter', duetInt(42n));
    assert.ok(duetValueEquals((await duet.get('counter')) as DuetValue, duetInt(42n)));
    const sub = await duet.subscribe('counter');
    assert.equal(sub.id, 7n);
    assert.ok(duetValueEquals(sub.snapshot as DuetValue, duetInt(42n)));
    await duet.unsubscribe(sub.id);

    // The exact bytes, with object keys in BYTE order rather than the order the
    // fields are written in. Rust's serde_json::Map is a BTreeMap, so that is
    // what the host emits and what the golden corpus records; a client that
    // emitted {kind, id, path} in declaration order would produce equivalent
    // JSON that is not byte-equal and would fail every byte-exact corpus case.
    assert.deepStrictEqual(transport.sent, [
      '{"id":"1","kind":"set","path":"counter","value":{"t":"i","v":"42"}}',
      '{"id":"2","kind":"get","path":"counter"}',
      '{"id":"3","kind":"subscribe","path":"counter"}',
      '{"id":"4","kind":"unsubscribe","subscription":"7"}',
    ]);
  });

  test('replies are correlated by the transport, not by the id', async () => {
    // The host answers the SECOND request first. The point is that this passes
    // with NO pending-id map in the client, because the transport's `send`
    // promise is already bound to that specific message's reply.
    const transport = new FakeTransport(async (request) => {
      const req = decodeRequestText(request);
      const path = formatDuetPath((req as { path: import('../src/index.ts').DuetPath }).path);
      await delay(path === 'slow' ? 60 : 5);
      return `{"kind":"value","id":"${req.id.toString()}","value":{"t":"s","v":"${path}"}}`;
    });

    const duet = new DuetClient(transport);
    const slow = duet.get('slow');
    const fast = duet.get('fast');
    assert.ok(duetValueEquals((await fast) as DuetValue, duetStr('fast')));
    assert.ok(duetValueEquals((await slow) as DuetValue, duetStr('slow')));
  });

  test('the client parses the real host bytes', async () => {
    // Every string below is VERBATIM stdout from the real Rust host
    // (duet_protocol::handle_text), captured from an actual run — so a
    // byte-for-byte change on either side of the wire fails here even if every
    // hand-written fixture above still passes.
    const replies = [
      '{"id":"1","kind":"done"}',
      '{"id":"2","kind":"value","value":{"t":"i","v":"42"}}',
      '{"id":"3","kind":"subscribed","snapshot":{"t":"i","v":"42"},"subscription":"0"}',
    ];
    let n = 0;
    const duet = new DuetClient(new FakeTransport(() => Promise.resolve(replies[n++] as string)));

    await duet.set('counter', duetInt(42n));
    assert.ok(duetValueEquals((await duet.get('counter')) as DuetValue, duetInt(42n)));
    const sub = await duet.subscribe('counter');
    assert.equal(sub.id, 0n);
    assert.ok(duetValueEquals(sub.snapshot as DuetValue, duetInt(42n)));
  });

  test('a failed response becomes a DuetFailure on that call only', async () => {
    const duet = new DuetClient(
      new FakeTransport((request) => {
        const req = decodeRequestText(request);
        return Promise.resolve(
          req.kind === 'get'
            ? `{"kind":"failed","id":"${req.id.toString()}","message":"no such path"}`
            : `{"kind":"done","id":"${req.id.toString()}"}`,
        );
      }),
    );

    await assert.rejects(() => duet.get('nope'), DuetFailure);
    // The client is still usable afterwards; the failure was that call's.
    await assert.doesNotReject(() => duet.set('a', duetInt(1n)));
  });

  test('a response of the wrong kind names what the caller wanted', async () => {
    const duet = new DuetClient(
      new FakeTransport(() => Promise.resolve('{"id":"1","kind":"done"}')),
    );
    await assert.rejects(() => duet.get('a'), (error: unknown) => {
      assert.ok(error instanceof DuetTransportError);
      assert.match(error.message, /expected a "value" response/);
      return true;
    });
  });

  test('a mis-correlated reply is caught, not handed to the caller', async () => {
    const duet = new DuetClient(
      new FakeTransport(() => Promise.resolve('{"id":"99","kind":"done"}')),
    );
    await assert.rejects(() => duet.set('a', duetInt(1n)), (error: unknown) => {
      assert.ok(error instanceof DuetTransportError);
      assert.match(error.message, /answered request 99/);
      return true;
    });
  });

  test("an uncorrelated failure carries the host's account of what went wrong", async () => {
    // `{"kind":"failed","id":"0"}` is the host saying it could not read the id
    // of the request it is refusing (`RequestId::UNCORRELATED` in
    // crates/duet-protocol/src/message.rs). A lone UTF-16 surrogate anywhere in
    // the message produces exactly this.
    //
    // The call must settle — it does here, because this transport resolves each
    // send with that message's own reply — but reporting only the id mismatch
    // would throw away the `message`, which is the sole explanation of *why*.
    // A developer left with "the host answered request 0" has a correlation
    // complaint and no diagnosis.
    const duet = new DuetClient(
      new FakeTransport(() =>
        Promise.resolve(
          '{"id":"0","kind":"failed","message":"malformed JSON: lone leading surrogate"}',
        ),
      ),
    );
    await assert.rejects(
      () => duet.get('a'),
      (error: unknown) => {
        assert.ok(error instanceof DuetTransportError);
        assert.match(error.message, /answered request 0/);
        assert.match(error.message, /lone leading surrogate/);
        return true;
      },
    );
  });

  test('no host listening resolves with null, not an exception from below', async () => {
    const duet = new DuetClient(FakeTransport.silent());
    await assert.rejects(() => duet.get('a'), (error: unknown) => {
      assert.ok(error instanceof DuetTransportError);
      assert.match(error.message, /no host is listening/);
      return true;
    });
  });

  test('an unparseable path fails before anything is sent', async () => {
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);
    await assert.rejects(() => duet.get('a.[0]'), DuetCodecError);
    assert.deepStrictEqual(transport.sent, [], 'a typo must cost no round trip');
  });
});

describe('the wire id domain', () => {
  for (const bad of ['007', '+1', '', '1 ', '9223372036854775808']) {
    test(`a non-canonical or out-of-domain response id (${JSON.stringify(bad)}) is rejected`, async () => {
      const duet = new DuetClient(
        new FakeTransport(() => Promise.resolve(`{"kind":"done","id":"${bad}"}`)),
      );
      await assert.rejects(() => duet.set('a', duetInt(1n)), DuetCodecError);
    });
  }

  test('a non-canonical subscription id is rejected', async () => {
    const duet = new DuetClient(
      new FakeTransport(
        () => Promise.resolve('{"kind":"subscribed","id":"1","subscription":"007","snapshot":null}'),
      ),
    );
    await assert.rejects(() => duet.subscribe('a'), DuetCodecError);
  });
});

describe('pushes', () => {
  const NOTIFICATION =
    '{"kind":"notification","notification":{"patch":{"path":"editor.zoom",' +
    '"value":{"t":"f","v":1.5}},"subscriber":"1","subscription":"2"}}';

  test('an unsolicited push reaches onPush', () => {
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);
    const seen: DuetNotification[] = [];
    duet.onPush = (note) => seen.push(note);
    duet.start();

    transport.deliverPush(NOTIFICATION);

    assert.equal(seen.length, 1);
    assert.equal(formatDuetPath((seen[0] as DuetNotification).path), 'editor.zoom');
    assert.equal((seen[0] as DuetNotification).subscription, 2n);
  });

  test('nothing arrives until start, and nothing after stop', () => {
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);
    let seen = 0;
    duet.onPush = () => seen++;

    assert.ok(!transport.isListening);
    transport.deliverPush(NOTIFICATION);
    assert.equal(seen, 0, 'nothing arrives before start()');

    duet.start();
    assert.ok(transport.isListening);
    transport.deliverPush(NOTIFICATION);
    assert.equal(seen, 1);

    duet.stop();
    assert.ok(!transport.isListening);
    transport.deliverPush(NOTIFICATION);
    assert.equal(seen, 1, 'nothing arrives after stop()');
  });

  test('a malformed push does not throw out of the handler', () => {
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);
    let seen = 0;
    duet.onPush = () => seen++;
    duet.start();

    // A push is fire-and-forget: there is no request id to fail against, so the
    // only sound response to a malformed push is to drop it. None of these may
    // escape — not the JSON.parse SyntaxError, not the depth guard, not a
    // missing field.
    for (const bad of [
      'not json',
      '[]',
      'null',
      '{"kind":"nope"}',
      '{"kind":"notification"}',
      '{"kind":"notification","notification":{"patch":{"path":"a.[0]","value":{"t":"n"}},"subscriber":"1","subscription":"2"}}',
      '['.repeat(50_000) + ']'.repeat(50_000),
    ]) {
      assert.doesNotThrow(() => transport.deliverPush(bad), `a push of ${bad.slice(0, 30)} escaped`);
    }
    assert.equal(seen, 0);
  });

  test('an exception from the application handler is not swallowed', () => {
    // Swallowing a bug in code this package does not own would hide it in the
    // one place a developer would never think to look.
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);
    duet.onPush = () => {
      throw new Error('application bug');
    };
    duet.start();
    assert.throws(() => transport.deliverPush(NOTIFICATION), /application bug/);
  });
});

describe('invoke', () => {
  test('sends canonical bytes and returns the exact value', async () => {
    const transport = new FakeTransport(echoHost);
    const duet = new DuetClient(transport);

    // Arguments built in an order that is NOT their canonical one, so the
    // encoder's sort is what produces the bytes below rather than the caller's
    // insertion order.
    const outcome = await duet.invoke(
      'subtract',
      new Map<string, DuetValue>([
        ['b', duetInt(3n)],
        ['a', duetInt(10n)],
      ]),
    );

    assert.deepStrictEqual(
      outcome,
      { kind: 'returned', value: duetInt(7n) },
      '10 - 3; a client that swapped the two argument names would answer -7 here',
    );
    assert.deepStrictEqual(transport.sent, [
      '{"args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}},' +
        '"command":"subtract","id":"1","kind":"invoke"}',
    ]);
  });

  test('a command taking nothing sends an empty tagged map', async () => {
    // Empty, not absent: `args` is a required field, and a host decoding this
    // message refuses it outright if it is missing — see
    // `envelope/request/invoke_without_args` in the wire corpus.
    const transport = new FakeTransport(echoHost);
    const outcome = await new DuetClient(transport).invoke('nothing');

    assert.deepStrictEqual(outcome, { kind: 'returned', value: duetNull() });
    assert.deepStrictEqual(transport.sent, [
      '{"args":{"t":"m","v":{}},"command":"nothing","id":"1","kind":"invoke"}',
    ]);
  });

  test('a raised error arrives typed, not as prose', async () => {
    // THE distinction this whole result type exists for. The error is a
    // structured value a caller can match on — a `failed` would have handed over
    // a sentence, and a sentence does not decode.
    const transport = new FakeTransport(echoHost);
    const outcome = await new DuetClient(transport).invoke('raise');

    assert.equal(outcome.kind, 'raised');
    if (outcome.kind !== 'raised') return;
    assert.ok(
      duetValueEquals(
        outcome.error,
        duetMap([
          ['code', duetStr('insufficient_funds')],
          ['short_by', duetInt(250n)],
        ]),
      ),
    );
    // ...and the structure really is reachable, not merely equal to a literal
    // written the same way.
    assert.equal(outcome.error.kind, 'map');
    if (outcome.error.kind !== 'map') return;
    assert.deepStrictEqual(outcome.error.entries.get('short_by'), duetInt(250n));
  });

  test('a refusal throws, so it can never be mistaken for a raise', async () => {
    // `raised` means the command ran and failed; `failed` means it never ran. A
    // client that surfaced both the same way would leave a caller unable to
    // decide whether retrying is safe.
    const transport = new FakeTransport(echoHost);
    await assert.rejects(
      () => new DuetClient(transport).invoke('nope'),
      (error: Error) =>
        error instanceof DuetFailure &&
        error.message === 'no command named "nope" is registered for this surface',
    );
  });

  test('a reply that answers no invoke is a transport failure', async () => {
    // Not a `DuetFailure`: the host did not refuse anything, it answered with a
    // kind that cannot be an answer to an `invoke` at all.
    const transport = new FakeTransport(echoHost);
    await assert.rejects(
      () => new DuetClient(transport).invoke('confused'),
      (error: Error) => error instanceof DuetTransportError,
    );
  });

  test('an invocation is matched exhaustively', () => {
    // The compile-time half of the claim. The `never` binding in the default arm
    // is what makes it one: an arm added to `DuetInvocation` and not handled here
    // makes `rest` a non-`never` type and fails `npm run typecheck`. A caller
    // cannot silently treat a command's error as a success.
    const describe = (outcome: DuetInvocation): string => {
      switch (outcome.kind) {
        case 'returned':
          return `returned ${outcome.value.kind}`;
        case 'raised':
          return `raised ${outcome.error.kind}`;
        default: {
          const rest: never = outcome;
          return rest;
        }
      }
    };
    assert.equal(describe({ kind: 'returned', value: duetInt(1n) }), 'returned int');
    assert.equal(describe({ kind: 'raised', error: duetNull() }), 'raised null');
  });

  test('every request and response kind is matched exhaustively', () => {
    // The same claim for the two envelope unions, which `invoke`, `returned` and
    // `raised` have just widened. Same `never` mechanism, so a variant added
    // without a case here fails the typecheck rather than going unhandled.
    const requestKind = (r: DuetRequest): string => {
      switch (r.kind) {
        case 'get':
        case 'set':
        case 'subscribe':
        case 'unsubscribe':
        case 'invoke':
          return r.kind;
        default: {
          const rest: never = r;
          return rest;
        }
      }
    };
    const responseKind = (r: DuetResponse): string => {
      switch (r.kind) {
        case 'value':
        case 'done':
        case 'subscribed':
        case 'failed':
        case 'returned':
        case 'raised':
          return r.kind;
        default: {
          const rest: never = r;
          return rest;
        }
      }
    };

    assert.deepStrictEqual(
      [
        requestKind({ kind: 'get', id: 1n, path: DUET_ROOT_PATH }),
        requestKind({ kind: 'set', id: 2n, path: DUET_ROOT_PATH, value: duetNull() }),
        requestKind({ kind: 'subscribe', id: 3n, path: DUET_ROOT_PATH }),
        requestKind({ kind: 'unsubscribe', id: 4n, subscription: 1n }),
        requestKind({ kind: 'invoke', id: 5n, command: 'c', args: new Map() }),
      ],
      ['get', 'set', 'subscribe', 'unsubscribe', 'invoke'],
    );
    assert.deepStrictEqual(
      [
        responseKind({ kind: 'value', id: 1n, value: null }),
        responseKind({ kind: 'done', id: 2n }),
        responseKind({ kind: 'subscribed', id: 3n, subscription: 1n, snapshot: null }),
        responseKind({ kind: 'failed', id: 4n, message: 'no' }),
        responseKind({ kind: 'returned', id: 5n, value: duetNull() }),
        responseKind({ kind: 'raised', id: 6n, error: duetNull() }),
      ],
      ['value', 'done', 'subscribed', 'failed', 'returned', 'raised'],
    );
  });
});

test('the channel name is defined once', () => {
  assert.equal(DUET_CHANNEL_NAME, 'duet/rpc');
});
