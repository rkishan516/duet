/**
 * The `wry` webview adapter: a {@link DuetTransport} over
 * `window.ipc.postMessage`.
 *
 * This is the one module that knows anything about a webview. Everything else
 * in the package is transport-agnostic and runs anywhere JavaScript does.
 *
 * # The shape of the channel this bridges
 *
 * `wry`'s IPC is **one-way**. A guest posts text with
 * `window.ipc.postMessage(...)`, which arrives at the Rust `ipc_handler`; the
 * host replies by *evaluating JavaScript back into the page* —
 * `window.__duet && window.__duet.onResponse({...});` — see `response_script`
 * and `push_script` in crates/duet-webview/src/lib.rs.
 *
 * Two consequences drive this file:
 *
 * 1. **There is no per-message reply path**, so this transport builds its own
 *    correlation on the envelope's `id`, exactly as
 *    {@link DuetTransport.send}'s contract requires of a transport that cannot
 *    correlate natively.
 * 2. **The host hands us a parsed object, not text.** The reply arrives as a
 *    JavaScript object literal inside an evaluated script, where the rest of
 *    this package speaks text. {@link stringifyHostMessage} bridges the two.
 *
 * @module
 */

import { DuetClient } from './client.ts';
import { DuetTransportError } from './errors.ts';
import type { DuetTransport } from './transport.ts';

/** `wry`'s injected IPC object. */
export interface WryIpc {
  /** Delivers `message` to the Rust `ipc_handler` as a string. */
  postMessage(message: string): void;
}

/**
 * The hooks the Rust host calls back into.
 *
 * The names are a contract with crates/duet-webview/src/lib.rs. If either side
 * renames one, every reply is silently dropped and the guest hangs forever —
 * there is no error, just silence.
 */
export interface DuetWebviewBridge {
  /** Receives one response, as the parsed object the host's script passes. */
  onResponse(response: unknown): void;
  /** Receives one push, as the parsed object the host's script passes. */
  onPush(push: unknown): void;
}

/** The slice of `window` this adapter touches. */
export interface WryWindow {
  /** Injected by `wry`. Absent until the webview finishes initialising. */
  ipc?: WryIpc;
  /** Installed by {@link WryTransport}; read by the host's evaluated scripts. */
  __duet?: DuetWebviewBridge;
}

/**
 * A {@link DuetTransport} over a `wry` webview's IPC channel.
 *
 * Constructing one installs `window.__duet`, so it must happen before the host
 * sends anything. The host's scripts are written `window.__duet && ...`, so
 * anything sent earlier is dropped rather than throwing — which means a late
 * installation shows up as a hang, not as an error. Install it at page load.
 */
export class WryTransport implements DuetTransport {
  /** @inheritdoc */
  onPush: ((message: string) => void) | null = null;

  readonly #host: WryWindow;

  /**
   * Requests awaiting a reply, keyed by the canonical decimal `id` string the
   * request carried — the same spelling the host echoes back.
   */
  readonly #pending = new Map<string, (reply: string) => void>();

  /**
   * @param host the object carrying `ipc` and receiving `__duet`; defaults to
   *   `globalThis`, which is `window` in a webview. Injectable so the whole
   *   adapter is testable under `node --test` with a plain object.
   */
  constructor(host: WryWindow = globalThis as unknown as WryWindow) {
    this.#host = host;
    host.__duet = {
      onResponse: (response: unknown): void => {
        this.#handleResponse(response);
      },
      onPush: (push: unknown): void => {
        this.onPush?.(stringifyHostMessage(push));
      },
    };
  }

  /**
   * Posts `request` and resolves with the reply that carries the same `id`.
   *
   * The id is read back out of the text rather than passed alongside it,
   * because {@link DuetTransport} is deliberately text-in/text-out — the same
   * two-member seam the Dart package uses. The text was produced by
   * `encodeRequestText`, so the parse cannot fail in practice; it is still
   * checked, because an unchecked assumption here would surface as a permanent
   * hang rather than an error.
   *
   * The returned promise never rejects on its own and never times out: a host
   * that never answers leaves it pending forever, which is the honest
   * representation of that state. Callers that need a deadline should race it.
   */
  send(request: string): Promise<string | null> {
    const id = readRequestId(request);
    const ipc = this.#host.ipc;
    if (ipc === undefined) {
      // Resolving with null rather than rejecting: "no host is listening" is
      // the exact condition `DuetTransport.send` documents a null for, and
      // `DuetClient` turns it into a DuetTransportError naming the request.
      return Promise.resolve(null);
    }
    return new Promise<string>((resolve) => {
      this.#pending.set(id, resolve);
      ipc.postMessage(request);
    });
  }

  /**
   * Routes one response to the call that is waiting for it.
   *
   * A response whose id matches nothing pending is dropped. That happens for a
   * reply to a request this client never sent (a second guest sharing the page,
   * or a host bug), and there is no caller to hand it to.
   */
  #handleResponse(response: unknown): void {
    const text = stringifyHostMessage(response);
    const id = readReplyId(response);
    if (id === null) return;
    const resolve = this.#pending.get(id);
    if (resolve === undefined) return;
    this.#pending.delete(id);
    resolve(text);
  }
}

/**
 * Installs a {@link WryTransport}, wraps it in a started {@link DuetClient},
 * and returns the client.
 *
 * The one-liner a webview guest actually wants:
 *
 * ```ts
 * const duet = connectWryDuet();
 * await duet.set('editor.zoom', duetFloat(1.5));
 * ```
 */
export function connectWryDuet(host?: WryWindow): DuetClient {
  const client = new DuetClient(host === undefined ? new WryTransport() : new WryTransport(host));
  client.start();
  return client;
}

/**
 * Renders a host message the rest of the package can decode.
 *
 * The host may hand us either text or an already-parsed object, depending on
 * how the page was wired; both are accepted.
 *
 * # The replacer is not decoration
 *
 * `JSON.stringify(-0)` is `"0"`, so re-serialising a parsed object would drop
 * the sign of a negative-zero float payload with no error. The Rust host never
 * sends one — `duet-codec` encodes `-0.0` as the string sentinel `"-0"` — but a
 * third-party host writing `{"t":"f","v":-0.0}` is well within the format, and
 * a silent sign flip is the worst possible failure mode. Mapping `-0` to the
 * sentinel string is exactly what the value decoder expects, so the round trip
 * becomes lossless instead of nearly lossless.
 *
 * `Object.is(value, -0)` is the only correct test: `value === -0` is also true
 * for `+0` and would rewrite every zero.
 */
export function stringifyHostMessage(message: unknown): string {
  if (typeof message === 'string') return message;
  return JSON.stringify(message, (_key: string, value: unknown) =>
    typeof value === 'number' && Object.is(value, -0) ? '-0' : value,
  );
}

/**
 * Reads the `id` out of a request this package produced.
 *
 * @throws DuetTransportError if the text is not an object with a string `id`,
 *   which would mean the transport was handed something other than an encoded
 *   Duet request.
 */
function readRequestId(request: string): string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(request) as unknown;
  } catch {
    throw new DuetTransportError('a request this transport was asked to send is not JSON');
  }
  const id = readIdField(parsed);
  if (id === null) {
    throw new DuetTransportError('a request this transport was asked to send carries no string id');
  }
  return id;
}

/** Reads the `id` out of a host reply, or `null` if it has none. */
function readReplyId(reply: unknown): string | null {
  return typeof reply === 'string' ? readIdFromText(reply) : readIdField(reply);
}

function readIdFromText(text: string): string | null {
  try {
    return readIdField(JSON.parse(text) as unknown);
  } catch {
    return null;
  }
}

/**
 * Reads an own `id` property that is a string.
 *
 * `Object.hasOwn` rather than plain access, so nothing inherited from
 * `Object.prototype` can masquerade as a field the peer sent.
 */
function readIdField(json: unknown): string | null {
  if (json === null || typeof json !== 'object' || Array.isArray(json)) return null;
  if (!Object.hasOwn(json, 'id')) return null;
  const id = (json as { id?: unknown })['id'];
  return typeof id === 'string' ? id : null;
}
