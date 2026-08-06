// What both headless guest drivers need and neither of them owns.
//
// This fixture ships two drivers, and `lib/main.dart` picks between them at
// boot from a value the host wrote into the shared store:
//
//   lib/solo_driver.dart -> `cargo run -p duet-backend-macos --example flutter_state`
//   lib/duet_driver.dart -> `cargo run -p duet-backend-macos --example two_guests`
//
// Selecting by store value rather than by entry point is deliberate.
// `flutter build macos` compiles `lib/main.dart` into one `App.framework`,
// and both Rust examples boot that same framework; a second entry point would
// mean a second build whose output lands at the same path as the first and
// silently overwrites it. Reading [kModePath] costs one `get` the guest was
// going to make during its handshake anyway, and an absent or unrecognised
// value keeps the pre-existing solo behaviour — so `flutter_state.rs` needs
// no host-side change to keep working.
import 'dart:async';

import 'package:duet/duet.dart';
import 'package:flutter/widgets.dart';

/// One log prefix for every driver, so embedded output is greppable.
void guestLog(String s) => debugPrint('[duet_guest] $s');

/// The store path the host writes to choose a driver.
///
/// Absent, null, or anything other than [kDuetMode] means the solo driver —
/// the pre-existing behaviour, so a host that has never heard of this
/// mechanism gets exactly what it got before.
const String kModePath = 'mode';

/// The [kModePath] value that selects `lib/duet_driver.dart`.
const String kDuetMode = 'duet';

/// How long to wait for the host to register its `duet/rpc` handler before
/// giving up. There is no readiness signal on the channel: a
/// `BasicMessageChannel` with no handler on the other side completes with
/// `null` rather than throwing, so the only handshake available is to send a
/// harmless request and retry until something answers.
const Duration kHandshakeTimeout = Duration(seconds: 10);

/// Time between handshake attempts.
const Duration kHandshakeInterval = Duration(milliseconds: 25);

/// Polls the host with a harmless `get` until it answers, or
/// [kHandshakeTimeout] elapses.
///
/// Returns whether the host ever answered. A fixed sleep was the old approach
/// and is a guess; this is the same wait expressed as a condition, which also
/// makes a host that never registers its handler a reported failure rather
/// than a mysterious silence.
///
/// Reads [kModePath] specifically, so the one request that must happen anyway
/// is the one whose answer selects the driver. A `get` of a path that does not
/// exist is answered `{"kind":"value","value":null}`, not a failure, so this
/// is harmless against any host regardless of what it seeded.
Future<bool> awaitHost(DuetClient duet) async {
  final Stopwatch elapsed = Stopwatch()..start();
  while (elapsed.elapsed < kHandshakeTimeout) {
    try {
      await duet.get(kModePath);
      guestLog('host answered after ${elapsed.elapsedMilliseconds}ms');
      return true;
    } on DuetTransportException catch (_) {
      await Future<void>.delayed(kHandshakeInterval);
    }
  }
  return false;
}
