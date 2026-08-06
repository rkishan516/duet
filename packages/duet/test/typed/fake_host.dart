/// A Dart stand-in for `duet_core::Store` behind `duet_protocol::dispatch`.
///
/// The typed runtime's whole job is to stay in step with a host whose exact
/// refusals matter — a `set` under an absent struct **fails**, a `subscribe`
/// at the same path **succeeds** — so a fake that merely "does something
/// reasonable" would let the tests pass against behaviour the real host does
/// not have.
///
/// Two things keep this honest:
///
/// - The write rules and their exact refusal messages are transcribed from
///   `crates/duet-core/src/value.rs`, and `fake_host_test.dart` asserts the
///   three measured behaviours directly, so drift from the host fails a test
///   here rather than surfacing as a wrong assumption elsewhere.
/// - The write is implemented **recursively**, deliberately unlike
///   `duetValueWith`'s iterative rebuild. A test that drove the host through
///   the function under test could not disagree with it.
library;

import 'dart:async';

import 'package:duet/duet.dart';

/// One live subscription on the fake host.
final class FakeSubscription {
  /// Creates a subscription record.
  FakeSubscription(this.id, this.path);

  /// The host-allocated subscription id.
  final int id;

  /// The path this subscription watches.
  final DuetPath path;
}

/// Thrown internally when a write is refused; becomes a `failed` response.
final class _SetRefused implements Exception {
  _SetRefused(this.message);
  final String message;
}

/// A transport that answers as a real Duet host would.
final class FakeHost implements DuetTransport {
  /// Seeds the store with [root].
  FakeHost(this.root);

  /// The whole state tree.
  DuetValue root;

  /// Every live subscription, by id.
  final Map<int, FakeSubscription> subscriptions = <int, FakeSubscription>{};

  /// Every request this host was handed, decoded, in order.
  final List<DuetRequest> requests = <DuetRequest>[];

  /// Called after a `subscribe` has been registered but **before** its reply
  /// is returned.
  ///
  /// This is the only way to reproduce the early-arrival race: the host can
  /// notify a subscription it has just registered before the guest has read
  /// the reply that names it.
  void Function(FakeHost host, int subscription)? beforeSubscribedReply;

  /// When true, every `get` is refused, standing in for a host that has gone
  /// away mid-resync.
  bool refuseGets = false;

  /// When set, every `get` waits on it before being answered.
  ///
  /// Lets a test put a notification *inside* a resync's round trip, which is
  /// the only way to reach the code that decides which of the two answers is
  /// the fresher one.
  Completer<void>? holdGets;

  /// How many `get` requests this host has answered or refused.
  int getCount = 0;

  int _nextSubscription = 1;
  int _nextRequestId = 9000;
  void Function(String message)? _onPush;

  @override
  set onPush(void Function(String message)? handler) => _onPush = handler;

  /// True while a client is listening for pushes.
  bool get isListening => _onPush != null;

  @override
  Future<String?> send(String request) async {
    final DuetRequest req = DuetRequest.fromWireText(request);
    requests.add(req);
    if (req is DuetGetRequest) await holdGets?.future;
    return switch (req) {
      DuetGetRequest() => _handleGet(req),
      DuetSetRequest() => _handleSet(req),
      DuetSubscribeRequest() => _handleSubscribe(req),
      DuetUnsubscribeRequest() => _handleUnsubscribe(req),
    };
  }

  /// Writes [value] at [path] as some *other* guest would, notifying every
  /// overlapping subscription. Returns the refusal message, or null on
  /// success.
  String? write(String path, DuetValue value) {
    final DuetPath parsed = DuetPath.parse(path);
    try {
      root = _writtenInto(root, parsed.segments, 0, value, parsed.toString());
    } on _SetRefused catch (e) {
      return e.message;
    }
    notify(parsed, value);
    return null;
  }

  /// Pushes a patch to every subscription whose path overlaps [path].
  ///
  /// Mirrors `Store::set`: the patch carries the *written* path, never the
  /// receiving subscriber's own, and every overlapping subscription is
  /// notified whether or not the value actually changed.
  void notify(DuetPath path, DuetValue value) {
    for (final FakeSubscription sub in subscriptions.values.toList()) {
      if (sub.path.isPrefixOf(path) || path.isPrefixOf(sub.path)) {
        pushTo(sub.id, path, value);
      }
    }
  }

  /// Pushes one patch to one subscription id, whether or not it exists.
  void pushTo(int subscription, DuetPath path, DuetValue value) {
    _onPush?.call(
      DuetNotificationPush(
        DuetNotification(
          subscriber: 1,
          subscription: subscription,
          path: path,
          value: value,
        ),
      ).toWireText(),
    );
  }

  /// The value at [path], or null if there is no node there.
  DuetValue? valueAt(String path) =>
      _at(root, DuetPath.parse(path).segments, 0);

  String _handleGet(DuetGetRequest req) {
    getCount += 1;
    if (refuseGets) {
      return DuetFailedResponse(id: req.id, message: 'the host is gone')
          .toWireText();
    }
    return DuetValueResponse(id: req.id, value: _at(root, req.path.segments, 0))
        .toWireText();
  }

  String _handleSet(DuetSetRequest req) {
    try {
      root = _writtenInto(
        root,
        req.path.segments,
        0,
        req.value,
        req.path.toString(),
      );
    } on _SetRefused catch (e) {
      return DuetFailedResponse(id: req.id, message: e.message).toWireText();
    }
    notify(req.path, req.value);
    return DuetDoneResponse(id: req.id).toWireText();
  }

  String _handleSubscribe(DuetSubscribeRequest req) {
    final int id = _nextSubscription++;
    subscriptions[id] = FakeSubscription(id, req.path);
    beforeSubscribedReply?.call(this, id);
    return DuetSubscribedResponse(
      id: req.id,
      subscription: id,
      snapshot: _at(root, req.path.segments, 0),
    ).toWireText();
  }

  String _handleUnsubscribe(DuetUnsubscribeRequest req) {
    subscriptions.remove(req.subscription);
    return DuetDoneResponse(id: req.id).toWireText();
  }

  /// An unused request id, for tests that hand-build a push.
  int nextRequestId() => _nextRequestId++;

  /// Iterative read, mirroring `Value::get`.
  static DuetValue? _at(DuetValue node, List<DuetSegment> segments, int from) {
    DuetValue current = node;
    for (int i = from; i < segments.length; i++) {
      final DuetSegment segment = segments[i];
      if (current is DuetMap && segment is DuetKeySegment) {
        final DuetValue? child = current.entries[segment.key];
        if (child == null) return null;
        current = child;
      } else if (current is DuetList && segment is DuetIndexSegment) {
        if (segment.index < 0 || segment.index >= current.items.length) {
          return null;
        }
        current = current.items[segment.index];
      } else {
        return null;
      }
    }
    return current;
  }

  /// Recursive write, mirroring `Value::set` including its refusal messages.
  static DuetValue _writtenInto(
    DuetValue node,
    List<DuetSegment> segments,
    int from,
    DuetValue value,
    String path,
  ) {
    if (from == segments.length) return value;
    final DuetSegment segment = segments[from];
    final bool last = from == segments.length - 1;

    if (node is DuetMap && segment is DuetKeySegment) {
      final DuetValue? child = node.entries[segment.key];
      if (child == null) {
        // Only the *final* segment of a map path is inserted; an intermediate
        // one is a MissingKey refusal.
        if (!last) throw _SetRefused('no key exists at path "$path"');
        return DuetMap(<String, DuetValue>{...node.entries, segment.key: value});
      }
      return DuetMap(<String, DuetValue>{
        ...node.entries,
        segment.key: _writtenInto(child, segments, from + 1, value, path),
      });
    }

    if (node is DuetList && segment is DuetIndexSegment) {
      if (segment.index < 0 || segment.index >= node.items.length) {
        throw _SetRefused(
          'index ${segment.index} is out of bounds at path "$path" '
          '(length ${node.items.length})',
        );
      }
      final List<DuetValue> items = <DuetValue>[...node.items];
      items[segment.index] =
          _writtenInto(items[segment.index], segments, from + 1, value, path);
      return DuetList(items);
    }

    throw _SetRefused('path "$path" addresses the wrong kind of node');
  }
}
