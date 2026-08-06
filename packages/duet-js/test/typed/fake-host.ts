/**
 * A TypeScript stand-in for `duet_core::Store` behind `duet_protocol::dispatch`.
 *
 * Mirrors `packages/duet/test/typed/fake_host.dart`.
 *
 * The typed runtime's whole job is to stay in step with a host whose exact
 * refusals matter — a `set` under an absent struct **fails**, a `subscribe` at
 * the same path **succeeds** — so a fake that merely "does something reasonable"
 * would let the tests pass against behaviour the real host does not have.
 *
 * Two things keep this honest:
 *
 * - The write rules and their exact refusal messages are transcribed from
 *   `crates/duet-core/src/value.rs`, and `option.test.ts` asserts the three
 *   measured behaviours directly, so drift from the host fails a test there
 *   rather than surfacing as a wrong assumption elsewhere.
 * - The write is implemented **recursively**, deliberately unlike
 *   `duetValueWith`'s iterative rebuild. A test that drove the host through the
 *   function under test could not disagree with it.
 *
 * @module
 */

import {
  decodeRequestText,
  encodePushText,
  encodeResponseText,
  formatDuetPath,
  parseDuetPath,
  type DuetPath,
  type DuetRequest,
  type DuetSegment,
  type DuetTransport,
  type DuetValue,
} from '../../src/index.ts';

/** One live subscription on the fake host. */
export interface FakeSubscription {
  readonly id: bigint;
  readonly path: DuetPath;
}

/** Thrown internally when a write is refused; becomes a `failed` response. */
class SetRefused extends Error {}

/** A transport that answers as a real Duet host would. */
export class FakeHost implements DuetTransport {
  /** The whole state tree. */
  root: DuetValue;

  /** Every live subscription, by id. */
  readonly subscriptions = new Map<bigint, FakeSubscription>();

  /** Every request this host was handed, decoded, in order. */
  readonly requests: DuetRequest[] = [];

  /**
   * Called after a `subscribe` has been registered but **before** its reply is
   * returned.
   *
   * This is the only way to reproduce the early-arrival race: the host can
   * notify a subscription it has just registered before the guest has read the
   * reply that names it.
   */
  beforeSubscribedReply: ((host: FakeHost, subscription: bigint) => void) | null = null;

  /**
   * When true, every `get` is refused, standing in for a host that has gone away
   * mid-resync.
   */
  refuseGets = false;

  /**
   * When set, every `get` waits on it before being answered.
   *
   * Lets a test put a notification *inside* a resync's round trip, which is the
   * only way to reach the code that decides which of the two answers is the
   * fresher one.
   */
  holdGets: Promise<void> | null = null;

  /** How many `get` requests this host has answered or refused. */
  getCount = 0;

  onPush: ((message: string) => void) | null = null;

  #nextSubscription = 1n;

  constructor(root: DuetValue) {
    this.root = root;
  }

  /** True while a client is listening for pushes. */
  get isListening(): boolean {
    return this.onPush !== null;
  }

  async send(request: string): Promise<string | null> {
    const req = decodeRequestText(request);
    this.requests.push(req);
    switch (req.kind) {
      case 'get': {
        this.getCount += 1;
        if (this.holdGets !== null) await this.holdGets;
        if (this.refuseGets) {
          return encodeResponseText({ kind: 'failed', id: req.id, message: 'the host is gone' });
        }
        return encodeResponseText({
          kind: 'value',
          id: req.id,
          value: valueAt(this.root, req.path.segments, 0),
        });
      }
      case 'set': {
        try {
          this.root = writtenInto(
            this.root,
            req.path.segments,
            0,
            req.value,
            formatDuetPath(req.path),
          );
        } catch (error) {
          if (!(error instanceof SetRefused)) throw error;
          return encodeResponseText({ kind: 'failed', id: req.id, message: error.message });
        }
        this.notify(req.path, req.value);
        return encodeResponseText({ kind: 'done', id: req.id });
      }
      case 'subscribe': {
        const id = this.#nextSubscription++;
        this.subscriptions.set(id, { id, path: req.path });
        this.beforeSubscribedReply?.(this, id);
        return encodeResponseText({
          kind: 'subscribed',
          id: req.id,
          subscription: id,
          snapshot: valueAt(this.root, req.path.segments, 0),
        });
      }
      case 'unsubscribe': {
        this.subscriptions.delete(req.subscription);
        return encodeResponseText({ kind: 'done', id: req.id });
      }
    }
  }

  /**
   * Writes `value` at `path` as some *other* guest would, notifying every
   * overlapping subscription. Returns the refusal message, or `null` on success.
   */
  write(path: string, value: DuetValue): string | null {
    const parsed = parseDuetPath(path);
    try {
      this.root = writtenInto(this.root, parsed.segments, 0, value, formatDuetPath(parsed));
    } catch (error) {
      if (!(error instanceof SetRefused)) throw error;
      return error.message;
    }
    this.notify(parsed, value);
    return null;
  }

  /**
   * Pushes a patch to every subscription whose path overlaps `path`.
   *
   * Mirrors `Store::set`: the patch carries the *written* path, never the
   * receiving subscriber's own, and every overlapping subscription is notified
   * whether or not the value actually changed.
   */
  notify(path: DuetPath, value: DuetValue): void {
    for (const sub of [...this.subscriptions.values()]) {
      if (isPrefixOf(sub.path, path) || isPrefixOf(path, sub.path)) {
        this.pushTo(sub.id, path, value);
      }
    }
  }

  /** Pushes one patch to one subscription id, whether or not it exists. */
  pushTo(subscription: bigint, path: DuetPath, value: DuetValue): void {
    this.onPush?.(
      encodePushText({
        kind: 'notification',
        notification: { subscriber: 1n, subscription, path, value },
      }),
    );
  }

  /** The value at `path`, or `null` if there is no node there. */
  valueAt(path: string): DuetValue | null {
    return valueAt(this.root, parseDuetPath(path).segments, 0);
  }
}

/** Two-way prefix matching, mirroring `Path::overlaps`. */
function isPrefixOf(path: DuetPath, other: DuetPath): boolean {
  if (path.segments.length > other.segments.length) return false;
  return path.segments.every((segment, i) => {
    const mine = other.segments[i] as DuetSegment;
    return segment.kind === 'key'
      ? mine.kind === 'key' && mine.key === segment.key
      : mine.kind === 'index' && mine.index === segment.index;
  });
}

/** Iterative read, mirroring `Value::get`. */
function valueAt(
  node: DuetValue,
  segments: readonly DuetSegment[],
  from: number,
): DuetValue | null {
  let current = node;
  for (let i = from; i < segments.length; i++) {
    const segment = segments[i] as DuetSegment;
    if (current.kind === 'map' && segment.kind === 'key') {
      const child = current.entries.get(segment.key);
      if (child === undefined) return null;
      current = child;
    } else if (current.kind === 'list' && segment.kind === 'index') {
      if (segment.index < 0 || segment.index >= current.items.length) return null;
      current = current.items[segment.index] as DuetValue;
    } else {
      return null;
    }
  }
  return current;
}

/** Recursive write, mirroring `Value::set` including its refusal messages. */
function writtenInto(
  node: DuetValue,
  segments: readonly DuetSegment[],
  from: number,
  value: DuetValue,
  path: string,
): DuetValue {
  if (from === segments.length) return value;
  const segment = segments[from] as DuetSegment;
  const last = from === segments.length - 1;

  if (node.kind === 'map' && segment.kind === 'key') {
    const child = node.entries.get(segment.key);
    const entries = new Map(node.entries);
    if (child === undefined) {
      // Only the *final* segment of a map path is inserted; an intermediate one
      // is a MissingKey refusal.
      if (!last) throw new SetRefused(`no key exists at path "${path}"`);
      entries.set(segment.key, value);
      return { kind: 'map', entries };
    }
    entries.set(segment.key, writtenInto(child, segments, from + 1, value, path));
    return { kind: 'map', entries };
  }

  if (node.kind === 'list' && segment.kind === 'index') {
    if (segment.index < 0 || segment.index >= node.items.length) {
      throw new SetRefused(
        `index ${String(segment.index)} is out of bounds at path "${path}" ` +
          `(length ${String(node.items.length)})`,
      );
    }
    const items = [...node.items];
    items[segment.index] = writtenInto(
      items[segment.index] as DuetValue,
      segments,
      from + 1,
      value,
      path,
    );
    return { kind: 'list', items };
  }

  throw new SetRefused(`path "${path}" addresses the wrong kind of node`);
}
