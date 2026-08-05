// The single-guest driver: the Dart half of
// `cargo run -p duet-backend-macos --example flutter_state`.
//
// Previously the whole of `lib/main.dart`; moved here unchanged in behaviour
// when the fixture grew a second driver (see `lib/guest_support.dart` for why
// one `App.framework` carries both). NO runApp, NO Ticker, NO widget tree.
//
// Modeled on the scratch probe app's `lib/main.dart`, which measured this
// exact sequence working against a real engine: requests served, a `set`
// visible to Rust, and a push observed by this guest.
//
// # How this driver reports back to Rust
//
// There is no side channel and no log scraping. Everything this guest
// observes — how many pushes arrived, what the host replied to hostile
// input, the exact doubles it wrote — is published by writing one map at
// [kReportPath] over the very `duet/rpc` channel under test. The Rust example
// polls that path out of the shared store and asserts against it.
//
// Using the store itself as the reporting channel is deliberate: a report
// that arrives at all is already evidence the guest -> host direction works,
// and it keeps the proof from depending on any transport the proof does not
// already trust.
//
// # Stages
//
// The report's `stage` field is a coarse progress marker the Rust driver
// waits on, so the two sides are sequenced rather than raced:
//
//   ready  -> the host answered a request; nothing asserted yet
//   probed -> `counter` written, doubles and hostile replies recorded
//   armed  -> subscribed to `counter`; the host may now write to it
//   done   -> the driver finished; no further writes are coming
import 'dart:async';
import 'dart:convert';

import 'package:flutter/widgets.dart';

import 'duet_client.dart';
import 'duet_value.dart';
import 'guest_support.dart';

/// The one store path this driver publishes its whole observable state at.
///
/// A single path, rewritten in full on every update, rather than one path per
/// field: `Value::set` never creates intermediate nodes
/// (crates/duet-core/src/value.rs), so a nested `guest.pushes` would need the
/// host to have seeded `guest` as a map first. One whole-map write needs the
/// host to seed nothing but the key itself, and it also means the Rust side
/// only ever reads one self-consistent snapshot instead of several paths that
/// might disagree mid-update.
const String kReportPath = 'guest';

/// The longest host reply this driver echoes back inside its report.
///
/// The host's failure replies are ~100 bytes; the reason this bound exists at
/// all is the case it is here to catch. A host bug that echoed a guest's own
/// 1 MB path back into the failure message would produce a reply too large to
/// carry home inside a report — so it is truncated here and the *untruncated*
/// byte count is reported alongside it. The Rust side asserts on the count,
/// not on the text, so a truncated echo fails loudly rather than looking fine.
const int kMaxReplyEcho = 4096;

/// How long the driver stays alive after arming, so the host has room to
/// write, notify, and observe the resulting push before Dart stops updating.
const Duration kObserveWindow = Duration(seconds: 8);

/// One hostile exchange: what was sent, and what came back.
///
/// `requestBytes`/`replyBytes` are UTF-8 byte counts, not Dart string lengths,
/// so they mean the same thing on both sides of the channel — the Rust host
/// measures its reply with `str::len()`, which is also bytes.
@immutable
class _Exchange {
  const _Exchange({
    required this.requestBytes,
    required this.replyBytes,
    required this.reply,
  });

  /// Size of the raw text sent to the host.
  final int requestBytes;

  /// Size of the host's reply, or -1 if the host did not answer at all.
  final int replyBytes;

  /// The reply text, truncated to [kMaxReplyEcho] characters.
  final String reply;

  /// The tagged form the host reads back out of the shared store.
  DuetValue toValue() => DuetMap(<String, DuetValue>{
    'requestBytes': DuetInt(requestBytes),
    'replyBytes': DuetInt(replyBytes),
    'reply': DuetStr(reply),
  });
}

/// Everything this guest wants the host to know, as one immutable snapshot.
@immutable
class _Report {
  const _Report({
    this.stage = 'booting',
    this.pushes = 0,
    this.lastPushPath,
    this.lastPushValue,
    this.subscription = 0,
    this.subscribeSnapshot,
    this.doubles = const <String, double>{},
    this.hostile = const <String, _Exchange>{},
  });

  /// Coarse progress marker; see this file's header for the sequence.
  final String stage;

  /// How many pushes [DuetClient.onPush] has been handed.
  final int pushes;

  /// The path carried by the most recent push.
  final String? lastPushPath;

  /// The value carried by the most recent push, verbatim.
  final DuetValue? lastPushValue;

  /// The subscription id the host assigned, or 0 before subscribing.
  final int subscription;

  /// The snapshot the host returned with that subscription.
  final DuetValue? subscribeSnapshot;

  /// Doubles this guest wrote, by name. The whole point of carrying them here
  /// is that they cross the channel as `{"t":"f","v":<JSON number>}` and the
  /// host compares bit patterns, not decimal text.
  final Map<String, double> doubles;

  /// Hostile exchanges, by case name.
  final Map<String, _Exchange> hostile;

  _Report copyWith({
    String? stage,
    int? pushes,
    String? lastPushPath,
    DuetValue? lastPushValue,
    int? subscription,
    DuetValue? subscribeSnapshot,
    Map<String, double>? doubles,
    Map<String, _Exchange>? hostile,
  }) => _Report(
    stage: stage ?? this.stage,
    pushes: pushes ?? this.pushes,
    lastPushPath: lastPushPath ?? this.lastPushPath,
    lastPushValue: lastPushValue ?? this.lastPushValue,
    subscription: subscription ?? this.subscription,
    subscribeSnapshot: subscribeSnapshot ?? this.subscribeSnapshot,
    doubles: doubles ?? this.doubles,
    hostile: hostile ?? this.hostile,
  );

  /// The tagged form written at [kReportPath].
  DuetValue toValue() => DuetMap(<String, DuetValue>{
    'stage': DuetStr(stage),
    'pushes': DuetInt(pushes),
    'pushPath': lastPushPath == null
        ? const DuetNull()
        : DuetStr(lastPushPath!),
    'pushValue': lastPushValue ?? const DuetNull(),
    'subscription': DuetInt(subscription),
    'subscribeSnapshot': subscribeSnapshot ?? const DuetNull(),
    'doubles': DuetMap(<String, DuetValue>{
      for (final MapEntry<String, double> e in doubles.entries)
        e.key: DuetFloat(e.value),
    }),
    'hostile': DuetMap(<String, DuetValue>{
      for (final MapEntry<String, _Exchange> e in hostile.entries)
        e.key: e.value.toValue(),
    }),
  });
}

/// Runs the single-guest sequence against an already-handshaken host.
Future<void> runSoloGuest(DuetClient duet) async {
  _Report report = const _Report();

  duet.onPush = (DuetNotification n) {
    report = report.copyWith(
      pushes: report.pushes + 1,
      lastPushPath: n.path,
      lastPushValue: n.value,
    );
    guestLog('PUSH #${report.pushes} path=${n.path} value=${n.value}');
    // Republished immediately so the host can observe the count over the same
    // channel. Fire-and-forget: a push handler must not block the isolate on
    // a round trip, and the host polls rather than waiting on this write.
    unawaited(_publish(duet, report));
  };

  report = report.copyWith(stage: 'ready');
  await _publish(duet, report);

  try {
    await duet.set('counter', const DuetInt(42));
    guestLog('set counter = Int(42) OK');

    report = report.copyWith(
      stage: 'probed',
      doubles: _doubleProbes(),
      hostile: await _probeHostile(),
    );
    await _publish(duet, report);

    final DuetSubscription sub = await duet.subscribe('counter');
    guestLog('subscribed id=${sub.id} snapshot=${sub.snapshot}');
    report = report.copyWith(
      stage: 'armed',
      subscription: sub.id,
      subscribeSnapshot: sub.snapshot ?? const DuetNull(),
    );
    await _publish(duet, report);
  } on Object catch (e) {
    guestLog('ERROR $e');
    return;
  }

  await Future<void>.delayed(kObserveWindow);
  report = report.copyWith(stage: 'done');
  await _publish(duet, report);
  guestLog('FINAL pushes=${report.pushes}');
}

/// Writes the whole report at [kReportPath].
///
/// Never throws: this runs from a push handler as well as from the sequence
/// above, and a failed report is something the host observes as a missing
/// update — there is nothing useful this guest could do about it, and taking
/// the isolate down would destroy the very evidence the host is waiting for.
Future<void> _publish(DuetClient duet, _Report report) async {
  try {
    await duet.set(kReportPath, report.toValue());
  } on Object catch (e) {
    guestLog('ERROR publishing the report failed: $e');
  }
}

/// The doubles this guest writes for the host to compare bit-for-bit.
///
/// Every one of these is chosen because its shortest decimal representation is
/// long, extreme, or both — exactly the inputs a JSON float parser without a
/// correctly-rounding slow path gets wrong. `0.1 + 0.2` is the classic
/// (0.30000000000000004, 17 significant digits); `maxFinite` and `minPositive`
/// sit at the two ends of the exponent range, and `minPositive` is a subnormal
/// whose decimal form (5e-324) has no exact binary counterpart at all;
/// `negativeZero` checks that a sign bit with no magnitude survives, which
/// naive `== 0.0` handling silently loses.
Map<String, double> _doubleProbes() => <String, double>{
  'sum': 0.1 + 0.2,
  'third': 1.0 / 3.0,
  'maxFinite': double.maxFinite,
  'minPositive': double.minPositive,
  'negativeZero': -0.0,
};

/// Sends malformed and oversized text RAW over the real channel, bypassing
/// [DuetClient]'s encoding entirely, and records what came back.
///
/// Bypassing the client is the point: this proves the *host's* rejection path
/// is total even when the guest is not this client at all — a guest is a
/// separate renderer and nothing constrains what it puts on the wire.
Future<Map<String, _Exchange>> _probeHostile() async {
  final Map<String, String> cases = <String, String>{
    // An empty payload. `StringCodec` puts zero bytes on the wire, which the
    // engine hands the host as a nil `NSData*` — the host's null arm, not a
    // decode failure.
    'empty': '',
    'notJson': 'not json',
    'emptyObject': '{}',
    // Parseable as JSON and as a path, but unresolvable: `a` does not exist,
    // so this dies in `duet_core::SetError`, whose Display renders the path.
    // A host that echoed it would answer ~1 MB. Carries id 9 so the reply's
    // echoed id proves the request really was decoded before being refused.
    'megabytePath':
        '{"kind":"set","id":"9","path":"a.${'k' * 1000000}",'
        '"value":{"t":"i","v":"1"}}',
    // One byte over `FlutterSurface`'s 1 MiB inbound cap, so it must be
    // refused by the surface before `handle_text` ever sees it.
    'overCap': 'x' * (1024 * 1024 + 1),
  };

  final Map<String, _Exchange> results = <String, _Exchange>{};
  for (final MapEntry<String, String> probe in cases.entries) {
    final String text = probe.value;
    final String? reply = await kDuetChannel.send(text);
    final _Exchange exchange = _Exchange(
      requestBytes: utf8.encode(text).length,
      replyBytes: reply == null ? -1 : utf8.encode(reply).length,
      reply: reply == null
          ? ''
          : (reply.length <= kMaxReplyEcho
                ? reply
                : reply.substring(0, kMaxReplyEcho)),
    );
    guestLog(
      'HOSTILE ${probe.key}: ${exchange.requestBytes} bytes -> '
      '${exchange.replyBytes} byte reply: ${exchange.reply}',
    );
    results[probe.key] = exchange;
  }
  return results;
}
