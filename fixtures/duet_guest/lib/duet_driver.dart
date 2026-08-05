// The two-guest driver: the Dart half of
// `cargo run -p duet-backend-macos --example two_guests`.
//
// The sibling of `lib/solo_driver.dart`, and its opposite in one respect: the
// solo driver is the only guest in the process, while this one shares a
// process, a main thread, a `tao` event loop and one Rust `Runtime` with a
// live `wry` webview guest running JavaScript. Everything this driver
// publishes exists so the Rust side can assert what the *other* guest could
// and could not do to this one.
//
// # How this driver reports back to Rust
//
// Same rule as the solo driver, for the same reason: no side channel and no
// log scraping. Every observation is published as one map at
// [kDuetReportPath] over the very `duet/rpc` channel under test, and the Rust
// example polls that path out of the shared store. A report that arrives at
// all is already evidence the guest -> host direction works.
//
// A path distinct from the solo driver's `guest`, because in this example the
// store is genuinely shared with a second guest and every path needs an
// unambiguous owner.
//
// # Stages
//
// The report's `stage` field sequences the two sides:
//
//   wrote     -> [kFromDartPath] written; the webview guest may now read it
//   crossRead -> this guest has read the *webview's* write back out of the
//                shared store and published it verbatim as `crossRead`
//   armed     -> subscribed to [kBothPath] and [kDartOnlyPath]; the host may
//                now write to either
//   done      -> the observation window elapsed; no further updates coming
//
// # Why this driver never attacks anything
//
// The hostile half of this example — a guest trying to unsubscribe another
// guest's subscription by guessing its id — is driven entirely from the
// *webview* side, as raw `window.ipc.postMessage` text. This driver is the
// victim, and its only job there is to keep receiving its own pushes. Keeping
// the attacker on one side and the victim on the other is what makes the
// claim legible: nothing this file does could mask a failure of the host's
// ownership check.
import 'dart:async';

import 'package:flutter/widgets.dart';

import 'duet_client.dart';
import 'duet_value.dart';
import 'guest_support.dart';

/// The one store path this driver publishes its whole observable state at.
///
/// See `lib/solo_driver.dart`'s `kReportPath` for why a single whole-map write
/// rather than one path per field.
const String kDuetReportPath = 'dartGuest';

/// Where this guest writes, for the webview guest to read.
const String kFromDartPath = 'shared.fromDart';

/// Where the webview guest writes, for this guest to read.
const String kFromJsPath = 'shared.fromJs';

/// The value this guest writes at [kFromDartPath].
///
/// Must match `DART_WRITES` in
/// `crates/duet-backend-macos/examples/two_guests.rs`. Distinct from the
/// webview's own value so that "each guest read the other's key" cannot be
/// satisfied by a guest reading back its own write.
const int kDartWrites = 222;

/// The path *both* guests subscribe to. One write here must produce exactly
/// one push for each guest, each carrying that guest's own subscription id.
const String kBothPath = 'both';

/// The path only *this* guest subscribes to. A write here must not reach the
/// webview guest at all — that is the half of the isolation claim that would
/// still pass with no subscriber filtering whatsoever if it were omitted.
const String kDartOnlyPath = 'dartOnly';

/// How long to keep polling for the webview guest's write before giving up.
///
/// Generous, and bounded: the two guests boot concurrently and neither waits
/// for the other, so this poll is the only thing ordering them. A guest that
/// hung here forever would look identical to a slow one from Rust, which is
/// exactly the failure the Rust driver's own deadline exists to report.
const Duration kCrossReadTimeout = Duration(seconds: 30);

/// Time between cross-read attempts.
const Duration kCrossReadInterval = Duration(milliseconds: 25);

/// How long this driver stays alive after arming.
///
/// Much longer than the solo driver's window: the Rust side runs several
/// staged writes, an attack, and two teardowns after this guest arms, and this
/// guest must still be publishing when the last of them lands. The process
/// exits when the Rust example exits, so over-shooting costs nothing.
const Duration kDuetObserveWindow = Duration(seconds: 60);

/// The subscription id used before a real one has been assigned.
///
/// `-1`, not `0`: `Store::subscribe` allocates ids from **0**
/// (crates/duet-core/src/store.rs), so `0` is a perfectly real subscription id
/// and cannot double as "none". Getting this wrong would let a push carrying
/// subscription 0 match an unset field and pass an assertion it should fail.
const int kNoSubscription = -1;

/// Everything this guest wants the host to know, as one immutable snapshot.
@immutable
class _DuetReport {
  const _DuetReport({
    this.stage = 'booting',
    this.pushes = 0,
    this.lastPushPath,
    this.lastPushValue,
    this.lastPushSubscriber = kNoSubscription,
    this.lastPushSubscription = kNoSubscription,
    this.subBoth = kNoSubscription,
    this.subDartOnly = kNoSubscription,
    this.crossRead,
  });

  /// Coarse progress marker; see this file's header for the sequence.
  final String stage;

  /// How many pushes [DuetClient.onPush] has been handed, over this guest's
  /// whole life. The Rust side asserts exact values against it, so it must
  /// count *every* push, not per-path ones.
  final int pushes;

  /// The path carried by the most recent push.
  final String? lastPushPath;

  /// The value carried by the most recent push, verbatim.
  final DuetValue? lastPushValue;

  /// The `subscriber` field of the most recent push — the host's own id for
  /// this guest. Published so Rust can check a push addressed to the *other*
  /// guest never arrives here.
  final int lastPushSubscriber;

  /// The `subscription` field of the most recent push. Must always be one of
  /// [subBoth] or [subDartOnly]; anything else is another guest's.
  final int lastPushSubscription;

  /// This guest's subscription id for [kBothPath].
  final int subBoth;

  /// This guest's subscription id for [kDartOnlyPath].
  final int subDartOnly;

  /// What this guest read back from [kFromJsPath] — the *webview* guest's
  /// write — published verbatim so Rust can compare exact values rather than
  /// a boolean.
  final DuetValue? crossRead;

  _DuetReport copyWith({
    String? stage,
    int? pushes,
    String? lastPushPath,
    DuetValue? lastPushValue,
    int? lastPushSubscriber,
    int? lastPushSubscription,
    int? subBoth,
    int? subDartOnly,
    DuetValue? crossRead,
  }) => _DuetReport(
    stage: stage ?? this.stage,
    pushes: pushes ?? this.pushes,
    lastPushPath: lastPushPath ?? this.lastPushPath,
    lastPushValue: lastPushValue ?? this.lastPushValue,
    lastPushSubscriber: lastPushSubscriber ?? this.lastPushSubscriber,
    lastPushSubscription: lastPushSubscription ?? this.lastPushSubscription,
    subBoth: subBoth ?? this.subBoth,
    subDartOnly: subDartOnly ?? this.subDartOnly,
    crossRead: crossRead ?? this.crossRead,
  );

  /// The tagged form written at [kDuetReportPath].
  DuetValue toValue() => DuetMap(<String, DuetValue>{
    'stage': DuetStr(stage),
    'pushes': DuetInt(pushes),
    'pushPath': lastPushPath == null
        ? const DuetNull()
        : DuetStr(lastPushPath!),
    'pushValue': lastPushValue ?? const DuetNull(),
    'pushSubscriber': DuetInt(lastPushSubscriber),
    'pushSubscription': DuetInt(lastPushSubscription),
    'subBoth': DuetInt(subBoth),
    'subDartOnly': DuetInt(subDartOnly),
    'crossRead': crossRead ?? const DuetNull(),
  });
}

/// Runs the two-guest sequence against an already-handshaken host.
Future<void> runDuetGuest(DuetClient duet) async {
  _DuetReport report = const _DuetReport();

  duet.onPush = (DuetNotification n) {
    report = report.copyWith(
      pushes: report.pushes + 1,
      lastPushPath: n.path,
      lastPushValue: n.value,
      lastPushSubscriber: n.subscriber,
      lastPushSubscription: n.subscription,
    );
    guestLog(
      'PUSH #${report.pushes} path=${n.path} value=${n.value} '
      'subscriber=${n.subscriber} subscription=${n.subscription}',
    );
    // Fire-and-forget: a push handler must not block the isolate on a round
    // trip, and the host polls rather than waiting on this write.
    unawaited(_publish(duet, report));
  };

  try {
    await duet.set(kFromDartPath, const DuetInt(kDartWrites));
    guestLog('set $kFromDartPath = Int($kDartWrites) OK');
    report = report.copyWith(stage: 'wrote');
    await _publish(duet, report);

    final DuetValue? fromJs = await _awaitTheOtherGuestsWrite(duet);
    if (fromJs == null) {
      guestLog('ERROR the webview guest never wrote $kFromJsPath');
      report = report.copyWith(stage: 'crossReadTimedOut');
      await _publish(duet, report);
      return;
    }
    guestLog('read $kFromJsPath = $fromJs (written by the webview guest)');
    report = report.copyWith(stage: 'crossRead', crossRead: fromJs);
    await _publish(duet, report);

    final DuetSubscription both = await duet.subscribe(kBothPath);
    final DuetSubscription dartOnly = await duet.subscribe(kDartOnlyPath);
    guestLog('subscribed $kBothPath=${both.id} $kDartOnlyPath=${dartOnly.id}');
    report = report.copyWith(
      stage: 'armed',
      subBoth: both.id,
      subDartOnly: dartOnly.id,
    );
    await _publish(duet, report);
  } on Object catch (e) {
    guestLog('ERROR $e');
    return;
  }

  await Future<void>.delayed(kDuetObserveWindow);
  report = report.copyWith(stage: 'done');
  await _publish(duet, report);
  guestLog('FINAL pushes=${report.pushes}');
}

/// Polls [kFromJsPath] until the webview guest has written an int there, or
/// [kCrossReadTimeout] elapses.
///
/// Returns the value verbatim, or `null` if it never appeared. Polling rather
/// than subscribing is deliberate: a subscription would make this guest's
/// cross-read depend on the very push-delivery path the rest of the example is
/// trying to prove, and the answer would then be circular.
///
/// The host seeds [kFromJsPath] as null, so "absent or null" is the
/// pre-write state and an int is unambiguously the webview guest's write.
Future<DuetValue?> _awaitTheOtherGuestsWrite(DuetClient duet) async {
  final Stopwatch elapsed = Stopwatch()..start();
  while (elapsed.elapsed < kCrossReadTimeout) {
    try {
      final DuetValue? seen = await duet.get(kFromJsPath);
      if (seen is DuetInt) return seen;
    } on Object catch (e) {
      guestLog('reading $kFromJsPath failed, retrying: $e');
    }
    await Future<void>.delayed(kCrossReadInterval);
  }
  return null;
}

/// Writes the whole report at [kDuetReportPath].
///
/// Never throws, for the same reason as the solo driver's publisher: this runs
/// from a push handler as well as from the sequence above, and taking the
/// isolate down would destroy the very evidence the host is waiting for.
Future<void> _publish(DuetClient duet, _DuetReport report) async {
  try {
    await duet.set(kDuetReportPath, report.toValue());
  } on Object catch (e) {
    guestLog('ERROR publishing the report failed: $e');
  }
}
