/// Routing host pushes to typed watchers.
library;

import 'dart:async';

import '../duet_client.dart';
import '../duet_error.dart';
import '../duet_message.dart';
import '../duet_path.dart';
import '../duet_value.dart';
import 'duet_merge.dart';
import 'duet_reading.dart';

/// How many early-arriving notifications a [DuetRouter] holds by default, and
/// how many subscription ids it will remember having dropped one for.
///
/// Small on purpose. The window it covers is a single `subscribe` round trip,
/// during which one path changing more than a handful of times means the
/// application is watching something far too hot for a typed mirror anyway.
const int defaultMaxBufferedPushes = 64;

/// Delivers the host's pushes to the typed watchers they belong to, keeping
/// each watcher's local mirror in step with the host.
///
/// One router per [DuetClient]. Generated code builds one and hands it to
/// every field it mints.
///
/// # The push slot has exactly one owner
///
/// `DuetClient.onPush` is a single mutable slot: a second assignment replaces
/// the first with no error and no warning, and the displaced owner simply
/// stops receiving notifications. Two routers — or a router and an application
/// that also wanted raw pushes — would silently steal each other's traffic,
/// and the symptom is a watcher that just stops updating, arbitrarily far from
/// the line that caused it. [attach] therefore refuses to install itself over
/// an existing owner. That turns a silent race into an error at the exact
/// point of the mistake.
final class DuetRouter {
  /// Wraps [client]. Nothing is delivered until [attach] runs.
  ///
  /// [maxBufferedPushes] bounds the early-arrival buffer; see [attach].
  DuetRouter(this.client, {this.maxBufferedPushes = defaultMaxBufferedPushes}) {
    if (maxBufferedPushes < 0) {
      throw ArgumentError.value(
        maxBufferedPushes,
        'maxBufferedPushes',
        'must not be negative',
      );
    }
  }

  /// The client whose pushes this router routes.
  final DuetClient client;

  /// The most notifications, and the most distinct subscription ids, this
  /// router will hold while waiting for their `subscribed` replies.
  final int maxBufferedPushes;

  /// Live watchers, keyed by the host's subscription id.
  ///
  /// **Id-keyed, not path-scanned.** The host stamps every notification with
  /// the subscription it answers, so the recipient is a map lookup. Matching
  /// on the path instead would be a linear scan of every watcher on every
  /// push, and — worse — would be *wrong*: overlapping watchers on `editor`
  /// and `editor.zoom` both match a write to `editor.zoom`, so a path scan
  /// cannot tell which subscription a notification answers, only which ones it
  /// could plausibly belong to. The host already answered that question.
  final Map<int, _Routed> _routes = <int, _Routed>{};

  /// Notifications that arrived before their subscription was registered.
  final Map<int, _EarlyArrivals> _early = <int, _EarlyArrivals>{};

  /// How many notifications [_early] currently holds, across all ids.
  int _buffered = 0;

  /// Set when a notification was dropped for an id [_early] had no room to
  /// even record. See [_bufferEarly].
  bool _unrecordedDrops = false;

  /// Resyncs in flight, so [settled] can wait for them.
  final Set<Future<void>> _inFlight = <Future<void>>{};

  bool _attached = false;

  /// True between [attach] and [detach].
  bool get isAttached => _attached;

  /// Takes ownership of [client]'s push slot and starts listening.
  ///
  /// Throws [StateError] if this router is already attached, or if anything
  /// else already owns the slot — see the note on this class for why that is
  /// an error rather than a silent overwrite.
  ///
  /// A `StateError` rather than a `DuetException`: that hierarchy is this
  /// package's statement about *untrusted input*, and every member of it is
  /// something a correct program still has to handle at runtime. Two owners
  /// for one slot is a bug in the calling code, which no runtime handler
  /// should paper over.
  void attach() {
    if (_attached) {
      throw StateError('this DuetRouter is already attached to its client');
    }
    if (client.onPush != null) {
      throw StateError(
        'another owner already holds this DuetClient.onPush slot; a second '
        'owner would silently take over its notifications. Use one router per '
        'client, and do not set onPush yourself while one is attached.',
      );
    }
    client.onPush = _receive;
    client.start();
    _attached = true;
  }

  /// Releases the push slot and stops listening.
  ///
  /// Live watchers are **not** cancelled: their subscriptions still exist on
  /// the host, and their owners are the ones holding the handles to close.
  /// Detaching only stops delivery, which is what makes it safe to call during
  /// teardown without needing every watcher in hand.
  ///
  /// Safe to call when not attached.
  void detach() {
    if (!_attached) return;
    _attached = false;
    client.onPush = null;
    client.stop();
    _early.clear();
    _buffered = 0;
    _unrecordedDrops = false;
  }

  /// Starts a typed watch of [path].
  ///
  /// [read] turns the raw mirror into a typed reading and must be total;
  /// `duetRequiredReading` and `duetOptionalReading` are. [onReading] is
  /// called for every notification after this future completes.
  ///
  /// The returned handle's `current` is already caught up when this returns:
  /// it reflects the host's snapshot **and** every notification that raced
  /// ahead of the `subscribed` reply. [onReading] is deliberately not called
  /// for those, so an application never receives a callback for a handle it
  /// does not yet hold.
  ///
  /// [path] must be a path `DuetPath.parse` produced: the subscription is sent
  /// as text, and a hand-built path with a key containing `.`, `[` or `]` does
  /// not round-trip.
  ///
  /// Throws whatever `DuetClient.subscribe` throws, and [StateError] if this
  /// router is not attached.
  Future<DuetWatch<T>> watch<T extends Object>(
    DuetPath path,
    DuetReading<T> Function(DuetValue? value) read,
    void Function(DuetReading<T> reading) onReading,
  ) async {
    if (!_attached) {
      throw StateError(
        'attach() this DuetRouter before watching, or its notifications will '
        'never arrive',
      );
    }
    final _Watch<T> route = _Watch<T>(this, path, read, onReading);
    final DuetSubscription subscription = await client.subscribe(route.pathText);
    final int id = subscription.id;

    // No `await` between here and `route.live = true`, so no push can be
    // observed against a half-registered watcher.
    route.id = id;
    _routes[id] = route;
    route.mirror = subscription.snapshot;
    route.refresh();

    final _EarlyArrivals? early = _early.remove(id);
    if (early != null) {
      _buffered -= early.notes.length;
      for (final DuetNotification note in early.notes) {
        _apply(route, note);
      }
    }
    if ((early?.lost ?? false) || _unrecordedDrops) {
      _scheduleResync(route);
    }

    route.live = true;
    return route;
  }

  /// Completes when every refetch this router has started has finished.
  ///
  /// A resync is asynchronous by nature — it is a `get` round trip — so
  /// without this there is no way to know when a watcher has stopped moving.
  /// Exposed rather than kept private because the alternative is a test (or a
  /// shutdown path) that waits a made-up number of milliseconds, which is how
  /// a suite ends up unable to reach the failure it was written for.
  Future<void> settled() async {
    while (_inFlight.isNotEmpty) {
      await Future.wait<void>(_inFlight.toList());
    }
  }

  /// Routes one notification, or buffers it if its subscription is not
  /// registered yet.
  void _receive(DuetNotification note) {
    final _Routed? route = _routes[note.subscription];
    if (route == null) {
      _bufferEarly(note);
      return;
    }
    _apply(route, note);
  }

  /// Holds a notification whose `subscribed` reply has not landed yet.
  ///
  /// # Bounded, and what happens at the bound
  ///
  /// A push genuinely can precede the reply that names its subscription: the
  /// host registers the subscription and can notify it before the guest has
  /// read the reply off the channel. Dropping those would lose changes with no
  /// trace.
  ///
  /// Buffering without a bound is not an option either — the buffer is fed by
  /// the peer, so an unbounded one is a memory exhaustion reachable by anything
  /// that can push. Two bounds apply: at most [maxBufferedPushes]
  /// notifications in total, and at most [maxBufferedPushes] distinct ids.
  ///
  /// At the bound nothing is *lost*, because nothing is silently discarded:
  /// the id is marked, and [watch] refetches that path instead of folding a
  /// sequence with a hole in it. If even the mark cannot be recorded — the id
  /// map is full too — [_unrecordedDrops] latches, and **every** subsequent
  /// registration refetches. That fallback is deliberately blunt: reaching it
  /// means the buffer is badly undersized for the workload, and one extra read
  /// per subscription is the cheapest possible price for never being wrong. It
  /// clears only on [detach].
  void _bufferEarly(DuetNotification note) {
    final _EarlyArrivals? existing = _early[note.subscription];
    if (existing != null) {
      if (_buffered >= maxBufferedPushes) {
        existing.lost = true;
        return;
      }
      existing.notes.add(note);
      _buffered += 1;
      return;
    }

    if (_early.length >= maxBufferedPushes) {
      _unrecordedDrops = true;
      return;
    }
    final _EarlyArrivals fresh = _EarlyArrivals();
    _early[note.subscription] = fresh;
    if (_buffered >= maxBufferedPushes) {
      fresh.lost = true;
      return;
    }
    fresh.notes.add(note);
    _buffered += 1;
  }

  /// Folds one notification into one watcher and reports the result.
  void _apply(_Routed route, DuetNotification note) {
    // Bumped for *every* notification, merged or not: the epoch is what a
    // resync in flight compares against to decide whether it is still the
    // freshest answer, and a notification it did not see makes it stale
    // whether or not the mirror moved.
    route.epoch += 1;
    final DuetMerge merge = duetMergeMirror(
      watched: route.path,
      mirror: route.mirror,
      changed: note.path,
      value: note.value,
    );
    switch (merge) {
      case DuetMerged(:final DuetValue? mirror):
        route.mirror = mirror;
        final bool decoded = route.refresh();
        // Scheduled *before* the application callback runs. A callback is
        // code this package does not own and is allowed to throw; recovery
        // must not be something an application bug can cancel.
        if (!decoded) _scheduleResync(route);
        _notify(route);
      case DuetResync():
        // Nothing is delivered here on purpose. The merge said this patch
        // cannot produce the watched path's new value, so the mirror is known
        // to be wrong; reporting it would be reporting a value this router has
        // already concluded is stale. The refetch delivers instead.
        _scheduleResync(route);
    }
  }

  /// Re-reads a watcher's path from the host, out of band.
  void _scheduleResync(_Routed route) {
    if (route.closed) return;
    final int epoch = route.epoch;
    late final Future<void> work;
    work = _resync(route, epoch).whenComplete(() => _inFlight.remove(work));
    _inFlight.add(work);
    unawaited(work);
  }

  Future<void> _resync(_Routed route, int epoch) async {
    final DuetValue? fresh;
    try {
      fresh = await client.get(route.pathText);
    } on DuetException catch (_) {
      // There is no truth to be had right now. Deliver the last known reading
      // rather than retrying: a host refusing reads would otherwise be asked
      // again for every notification, forever. The next successful
      // notification corrects the mirror.
      _notify(route);
      return;
    }
    // A notification that arrived while this read was in flight is strictly
    // fresher than its answer, and so is a second resync. Either bumps the
    // epoch, and this result is dropped rather than overwriting a newer one.
    if (route.closed || route.epoch != epoch) return;
    route.mirror = fresh;
    route.refresh();
    // Deliberately no second resync when this still does not decode. Another
    // guest is entitled to write any type to any path, so a value this codec
    // refuses may be exactly what the host holds — and refetching it would
    // return the same value forever, one round trip per attempt. The mismatch
    // is delivered instead, which is what a mismatch is for.
    _notify(route);
  }

  /// Calls a watcher's callback, unless it is closed or not yet handed out.
  void _notify(_Routed route) {
    if (route.closed || !route.live) return;
    route.notifyListener();
  }

  /// Cancels a watch. See `DuetWatch.close`.
  Future<void> _close(_Routed route) async {
    if (route.closed) return;
    route.closed = true;
    final int? id = route.id;
    if (id == null) return;
    _routes.remove(id);
    final _EarlyArrivals? early = _early.remove(id);
    if (early != null) _buffered -= early.notes.length;
    await client.unsubscribe(id);
  }
}

/// Notifications held for one subscription id that has not registered yet.
final class _EarlyArrivals {
  /// The notifications, in arrival order.
  final List<DuetNotification> notes = <DuetNotification>[];

  /// True once the bound stopped this id's notifications being recorded.
  bool lost = false;
}

/// The parts of a watch the router touches without knowing its type argument.
///
/// A non-generic base, so `_routes` can be one map rather than one per `T`.
abstract class _Routed {
  _Routed(this.router, this.path) : pathText = path.toString();

  final DuetRouter router;

  /// The path this watch mirrors.
  final DuetPath path;

  /// [path] rendered once, since every refetch needs it as text.
  final String pathText;

  /// The host's subscription id, once the `subscribed` reply has landed.
  int? id;

  /// The watched path's last known value; `null` means no node exists there.
  DuetValue? mirror;

  /// Bumped by every notification and by every resync, so an in-flight
  /// refetch can tell whether its answer is still the freshest one.
  int epoch = 0;

  /// False until `watch` has returned the handle to its caller.
  bool live = false;

  /// True once closed.
  bool closed = false;

  /// Recomputes the typed reading from [mirror]. Returns false on a mismatch.
  bool refresh();

  /// Hands the current reading to the application.
  void notifyListener();
}

final class _Watch<T extends Object> extends _Routed implements DuetWatch<T> {
  _Watch(super.router, super.path, this._read, this._onReading);

  final DuetReading<T> Function(DuetValue? value) _read;
  final void Function(DuetReading<T> reading) _onReading;

  /// Absent until the `subscribed` snapshot lands, which is the truth: this
  /// watch knows nothing about its path until the host has answered.
  DuetReading<T> _current = DuetAbsent<T>();

  @override
  DuetReading<T> get current => _current;

  @override
  bool get isClosed => closed;

  @override
  bool refresh() {
    _current = _read(mirror);
    return _current is! DuetMismatch<T>;
  }

  @override
  void notifyListener() => _onReading(_current);

  @override
  Future<void> close() => router._close(this);
}
