/**
 * Routing host pushes to typed watchers.
 *
 * Mirrors `packages/duet/lib/src/typed/duet_router.dart`.
 *
 * @module
 */

import type { DuetClient } from '../client.ts';
import { DuetError } from '../errors.ts';
import type { DuetNotification } from '../message.ts';
import { formatDuetPath, type DuetPath } from '../path.ts';
import type { DuetValue } from '../value.ts';
import { duetMergeMirror } from './merge.ts';
import { duetAbsent, type DuetReading, type DuetWatch } from './reading.ts';

/**
 * How many early-arriving notifications a {@link DuetRouter} holds by default,
 * and how many subscription ids it will remember having dropped one for.
 *
 * Small on purpose. The window it covers is a single `subscribe` round trip,
 * during which one path changing more than a handful of times means the
 * application is watching something far too hot for a typed mirror anyway.
 */
export const DEFAULT_MAX_BUFFERED_PUSHES = 64;

/** Notifications held for one subscription id that has not registered yet. */
interface EarlyArrivals {
  /** The notifications, in arrival order. */
  readonly notes: DuetNotification[];
  /** True once the bound stopped this id's notifications being recorded. */
  lost: boolean;
}

/**
 * The parts of a watch the router touches without knowing its type argument.
 *
 * A non-generic view, so the route table can be one map rather than one per `T`.
 */
interface Routed {
  /** The path this watch mirrors. */
  readonly path: DuetPath;
  /** `path` rendered once, since every refetch needs it as text. */
  readonly pathText: string;
  /** The host's subscription id, once the `subscribed` reply has landed. */
  id: bigint | null;
  /** The watched path's last known value; `null` means no node exists there. */
  mirror: DuetValue | null;
  /**
   * Bumped by every notification and by every resync, so an in-flight refetch
   * can tell whether its answer is still the freshest one.
   */
  epoch: number;
  /** False until `watch` has handed the handle to its caller. */
  live: boolean;
  /** True once closed. */
  closed: boolean;
  /** Recomputes the typed reading from `mirror`. Returns false on a mismatch. */
  refresh(): boolean;
  /** Hands the current reading to the application. */
  notifyListener(): void;
}

class Watch<T extends {}> implements DuetWatch<T>, Routed {
  readonly path: DuetPath;
  readonly pathText: string;
  id: bigint | null = null;
  mirror: DuetValue | null = null;
  epoch = 0;
  live = false;
  closed = false;

  /**
   * Absent until the `subscribed` snapshot lands, which is the truth: this watch
   * knows nothing about its path until the host has answered.
   */
  #current: DuetReading<T> = duetAbsent();
  readonly #read: (value: DuetValue | null) => DuetReading<T>;
  readonly #onReading: (reading: DuetReading<T>) => void;
  readonly #closeRoute: (route: Routed) => Promise<void>;

  constructor(
    path: DuetPath,
    read: (value: DuetValue | null) => DuetReading<T>,
    onReading: (reading: DuetReading<T>) => void,
    closeRoute: (route: Routed) => Promise<void>,
  ) {
    this.path = path;
    this.pathText = formatDuetPath(path);
    this.#read = read;
    this.#onReading = onReading;
    // A closure rather than a call back into the router, which is the one place
    // this file diverges from the Dart mirror: Dart's library privacy lets a
    // watch call `router._close(this)` directly, and TypeScript's `#private`
    // does not cross a class boundary. The behaviour is identical.
    this.#closeRoute = closeRoute;
  }

  get current(): DuetReading<T> {
    return this.#current;
  }

  get isClosed(): boolean {
    return this.closed;
  }

  refresh(): boolean {
    this.#current = this.#read(this.mirror);
    return this.#current.kind !== 'mismatch';
  }

  notifyListener(): void {
    this.#onReading(this.#current);
  }

  close(): Promise<void> {
    return this.#closeRoute(this);
  }
}

/**
 * Delivers the host's pushes to the typed watchers they belong to, keeping each
 * watcher's local mirror in step with the host.
 *
 * One router per {@link DuetClient}. Generated code builds one and hands it to
 * every field it mints.
 *
 * # The push slot has exactly one owner
 *
 * {@link DuetClient.onPush} is a single mutable slot: a second assignment
 * replaces the first with no error and no warning, and the displaced owner
 * simply stops receiving notifications. Two routers — or a router and an
 * application that also wanted raw pushes — would silently steal each other's
 * traffic, and the symptom is a watcher that just stops updating, arbitrarily
 * far from the line that caused it. {@link DuetRouter.attach} therefore refuses
 * to install itself over an existing owner. That turns a silent race into an
 * error at the exact point of the mistake.
 */
export class DuetRouter {
  /** The client whose pushes this router routes. */
  readonly client: DuetClient;

  /**
   * The most notifications, and the most distinct subscription ids, this router
   * will hold while waiting for their `subscribed` replies.
   */
  readonly maxBufferedPushes: number;

  /**
   * Live watchers, keyed by the host's subscription id.
   *
   * **Id-keyed, not path-scanned.** The host stamps every notification with the
   * subscription it answers, so the recipient is a map lookup. Matching on the
   * path instead would be a linear scan of every watcher on every push, and —
   * worse — would be *wrong*: overlapping watchers on `editor` and `editor.zoom`
   * both match a write to `editor.zoom`, so a path scan cannot tell which
   * subscription a notification answers, only which ones it could plausibly
   * belong to. The host already answered that question.
   */
  readonly #routes = new Map<bigint, Routed>();

  /** Notifications that arrived before their subscription was registered. */
  readonly #early = new Map<bigint, EarlyArrivals>();

  /** How many notifications `#early` currently holds, across all ids. */
  #buffered = 0;

  /**
   * Set when a notification was dropped for an id `#early` had no room to even
   * record. See {@link DuetRouter.attach}'s note on the bound.
   */
  #unrecordedDrops = false;

  /** Resyncs in flight, so {@link DuetRouter.settled} can wait for them. */
  readonly #inFlight = new Set<Promise<void>>();

  #attached = false;

  /**
   * Wraps `client`. Nothing is delivered until {@link DuetRouter.attach} runs.
   *
   * @param client the client whose pushes to route
   * @param maxBufferedPushes bounds the early-arrival buffer
   * @throws RangeError if `maxBufferedPushes` is negative
   */
  constructor(client: DuetClient, maxBufferedPushes = DEFAULT_MAX_BUFFERED_PUSHES) {
    if (maxBufferedPushes < 0) {
      throw new RangeError('maxBufferedPushes must not be negative');
    }
    this.client = client;
    this.maxBufferedPushes = maxBufferedPushes;
  }

  /** True between {@link DuetRouter.attach} and {@link DuetRouter.detach}. */
  get isAttached(): boolean {
    return this.#attached;
  }

  /**
   * Takes ownership of the client's push slot and starts listening.
   *
   * A plain `Error` rather than a {@link DuetError}: that hierarchy is this
   * package's statement about *untrusted input*, and every member of it is
   * something a correct program still has to handle at runtime. Two owners for
   * one slot is a bug in the calling code, which no runtime handler should paper
   * over.
   *
   * @throws Error if this router is already attached, or if anything else
   *   already owns the slot.
   */
  attach(): void {
    if (this.#attached) {
      throw new Error('this DuetRouter is already attached to its client');
    }
    if (this.client.onPush !== null) {
      throw new Error(
        'another owner already holds this DuetClient.onPush slot; a second ' +
          'owner would silently take over its notifications. Use one router per ' +
          'client, and do not set onPush yourself while one is attached.',
      );
    }
    this.client.onPush = (note: DuetNotification): void => {
      this.#receive(note);
    };
    this.client.start();
    this.#attached = true;
  }

  /**
   * Releases the push slot and stops listening.
   *
   * Live watchers are **not** cancelled: their subscriptions still exist on the
   * host, and their owners are the ones holding the handles to close. Detaching
   * only stops delivery, which is what makes it safe to call during teardown
   * without needing every watcher in hand.
   *
   * Safe to call when not attached.
   */
  detach(): void {
    if (!this.#attached) return;
    this.#attached = false;
    this.client.onPush = null;
    this.client.stop();
    this.#early.clear();
    this.#buffered = 0;
    this.#unrecordedDrops = false;
  }

  /**
   * Starts a typed watch of `path`.
   *
   * `read` turns the raw mirror into a typed reading and must be total;
   * {@link duetRequiredReading} and {@link duetOptionalReading} are. `onReading`
   * is called for every notification after this promise resolves.
   *
   * The returned handle's `current` is already caught up when this resolves: it
   * reflects the host's snapshot **and** every notification that raced ahead of
   * the `subscribed` reply. `onReading` is deliberately not called for those, so
   * an application never receives a callback for a handle it does not yet hold.
   *
   * `path` must be a path {@link parseDuetPath} produced: the subscription is
   * sent as text, and a hand-built path with a key containing `.`, `[` or `]`
   * does not round-trip.
   *
   * @throws Error if this router is not attached, plus whatever
   *   {@link DuetClient.subscribe} throws.
   */
  async watch<T extends {}>(
    path: DuetPath,
    read: (value: DuetValue | null) => DuetReading<T>,
    onReading: (reading: DuetReading<T>) => void,
  ): Promise<DuetWatch<T>> {
    if (!this.#attached) {
      throw new Error(
        'attach() this DuetRouter before watching, or its notifications will never arrive',
      );
    }
    const route = new Watch<T>(path, read, onReading, (closing) =>
      this.#closeRoute(closing),
    );
    const subscription = await this.client.subscribe(route.pathText);
    const id = subscription.id;

    // No `await` between here and `route.live = true`, so no push can be
    // observed against a half-registered watcher.
    route.id = id;
    this.#routes.set(id, route);
    route.mirror = subscription.snapshot;
    route.refresh();

    const early = this.#early.get(id);
    if (early !== undefined) {
      this.#early.delete(id);
      this.#buffered -= early.notes.length;
      for (const note of early.notes) this.#apply(route, note);
    }
    if ((early?.lost ?? false) || this.#unrecordedDrops) {
      this.#scheduleResync(route);
    }

    route.live = true;
    return route;
  }

  /**
   * Resolves when every refetch this router has started has finished.
   *
   * A resync is asynchronous by nature — it is a `get` round trip — so without
   * this there is no way to know when a watcher has stopped moving. Exposed
   * rather than kept private because the alternative is a test (or a shutdown
   * path) that waits a made-up number of milliseconds, which is how a suite ends
   * up unable to reach the failure it was written for.
   */
  async settled(): Promise<void> {
    while (this.#inFlight.size > 0) {
      await Promise.all([...this.#inFlight]);
    }
  }

  /**
   * Routes one notification, or buffers it if its subscription is not registered
   * yet.
   */
  #receive(note: DuetNotification): void {
    const route = this.#routes.get(note.subscription);
    if (route === undefined) {
      this.#bufferEarly(note);
      return;
    }
    this.#apply(route, note);
  }

  /**
   * Holds a notification whose `subscribed` reply has not landed yet.
   *
   * # Bounded, and what happens at the bound
   *
   * A push genuinely can precede the reply that names its subscription: the host
   * registers the subscription and can notify it before the guest has read the
   * reply off the channel. Dropping those would lose changes with no trace.
   *
   * Buffering without a bound is not an option either — the buffer is fed by the
   * peer, so an unbounded one is a memory exhaustion reachable by anything that
   * can push. Two bounds apply: at most `maxBufferedPushes` notifications in
   * total, and at most `maxBufferedPushes` distinct ids.
   *
   * At the bound nothing is *lost*, because nothing is silently discarded: the
   * id is marked, and {@link DuetRouter.watch} refetches that path instead of
   * folding a sequence with a hole in it. If even the mark cannot be recorded —
   * the id map is full too — a latch is set and **every** subsequent
   * registration refetches. That fallback is deliberately blunt: reaching it
   * means the buffer is badly undersized for the workload, and one extra read
   * per subscription is the cheapest possible price for never being wrong. It
   * clears only on {@link DuetRouter.detach}.
   */
  #bufferEarly(note: DuetNotification): void {
    const existing = this.#early.get(note.subscription);
    if (existing !== undefined) {
      if (this.#buffered >= this.maxBufferedPushes) {
        existing.lost = true;
        return;
      }
      existing.notes.push(note);
      this.#buffered += 1;
      return;
    }

    if (this.#early.size >= this.maxBufferedPushes) {
      this.#unrecordedDrops = true;
      return;
    }
    const fresh: EarlyArrivals = { notes: [], lost: false };
    this.#early.set(note.subscription, fresh);
    if (this.#buffered >= this.maxBufferedPushes) {
      fresh.lost = true;
      return;
    }
    fresh.notes.push(note);
    this.#buffered += 1;
  }

  /** Folds one notification into one watcher and reports the result. */
  #apply(route: Routed, note: DuetNotification): void {
    // Bumped for *every* notification, merged or not: the epoch is what a resync
    // in flight compares against to decide whether it is still the freshest
    // answer, and a notification it did not see makes it stale whether or not
    // the mirror moved.
    route.epoch += 1;
    const merge = duetMergeMirror(route.path, route.mirror, note.path, note.value);
    if (merge.kind === 'merged') {
      route.mirror = merge.mirror;
      const decoded = route.refresh();
      // Scheduled *before* the application callback runs. A callback is code
      // this package does not own and is allowed to throw; recovery must not be
      // something an application bug can cancel.
      if (!decoded) this.#scheduleResync(route);
      this.#notify(route);
      return;
    }
    // Nothing is delivered here on purpose. The merge said this patch cannot
    // produce the watched path's new value, so the mirror is known to be wrong;
    // reporting it would be reporting a value this router has already concluded
    // is stale. The refetch delivers instead.
    this.#scheduleResync(route);
  }

  /** Re-reads a watcher's path from the host, out of band. */
  #scheduleResync(route: Routed): void {
    if (route.closed) return;
    const epoch = route.epoch;
    const work = this.#resync(route, epoch).finally(() => {
      this.#inFlight.delete(work);
    });
    this.#inFlight.add(work);
  }

  async #resync(route: Routed, epoch: number): Promise<void> {
    let fresh: DuetValue | null;
    try {
      fresh = await this.client.get(route.pathText);
    } catch (error) {
      if (!(error instanceof DuetError)) throw error;
      // There is no truth to be had right now. Deliver the last known reading
      // rather than retrying: a host refusing reads would otherwise be asked
      // again for every notification, forever. The next successful notification
      // corrects the mirror.
      this.#notify(route);
      return;
    }
    // A notification that arrived while this read was in flight is strictly
    // fresher than its answer, and so is a second resync. Either bumps the
    // epoch, and this result is dropped rather than overwriting a newer one.
    if (route.closed || route.epoch !== epoch) return;
    route.mirror = fresh;
    route.refresh();
    // Deliberately no second resync when this still does not decode. Another
    // guest is entitled to write any type to any path, so a value this codec
    // refuses may be exactly what the host holds — and refetching it would
    // return the same value forever, one round trip per attempt. The mismatch is
    // delivered instead, which is what a mismatch is for.
    this.#notify(route);
  }

  /** Calls a watcher's callback, unless it is closed or not yet handed out. */
  #notify(route: Routed): void {
    if (route.closed || !route.live) return;
    route.notifyListener();
  }

  /** Cancels a watch. See {@link DuetWatch.close}. */
  async #closeRoute(route: Routed): Promise<void> {
    if (route.closed) return;
    route.closed = true;
    const id = route.id;
    if (id === null) return;
    this.#routes.delete(id);
    const early = this.#early.get(id);
    if (early !== undefined) {
      this.#early.delete(id);
      this.#buffered -= early.notes.length;
    }
    await this.client.unsubscribe(id);
  }
}
