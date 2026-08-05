// Headless duet guest entry point: NO runApp, NO Ticker, NO widget tree.
//
// This is a driver, not a library — it is what a Rust example boots inside a
// `FlutterEngine` to exercise [DuetClient] against the real Rust host. It is
// not exercised by `flutter test` (see test/duet_client_test.dart and
// test/rust_goldens_test.dart for that); it only runs embedded, or via
// `flutter run -d macos` for a human to watch the debug console.
//
// All this file does is shake hands with the host and choose a driver:
//
//   lib/solo_driver.dart -> the single-guest proof (`example flutter_state`)
//   lib/duet_driver.dart -> the two-guest proof   (`example two_guests`)
//
// See `lib/guest_support.dart` for why the choice is made from a value in the
// shared store rather than from a second entry point, and why an absent or
// unrecognised value keeps the solo behaviour.
import 'package:flutter/widgets.dart';

import 'duet_client.dart';
import 'duet_value.dart';
import 'duet_driver.dart';
import 'guest_support.dart';
import 'solo_driver.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  guestLog('dart main() running (headless: no runApp)');

  final DuetClient duet = DuetClient()..start();

  if (!await awaitHost(duet)) {
    guestLog('ERROR the host never answered on ${kDuetChannel.name}; giving up');
    return;
  }

  if (await _hostWantsTheDuetDriver(duet)) {
    guestLog('host mode is "$kDuetMode": running the two-guest driver');
    await runDuetGuest(duet);
  } else {
    guestLog('host mode is not "$kDuetMode": running the solo driver');
    await runSoloGuest(duet);
  }
}

/// Reads [kModePath] and reports whether the host asked for the two-guest
/// driver.
///
/// Total: any failure — a host that rejected the read, a value of the wrong
/// shape, a path that does not exist — falls back to the solo driver rather
/// than throwing. A guest that crashed here would take the isolate down before
/// either driver ran, which is a far worse outcome than running the wrong one:
/// the Rust side asserts on what the driver publishes and would fail loudly
/// either way.
Future<bool> _hostWantsTheDuetDriver(DuetClient duet) async {
  try {
    final DuetValue? mode = await duet.get(kModePath);
    return mode is DuetStr && mode.value == kDuetMode;
  } on Object catch (e) {
    guestLog('ERROR reading "$kModePath", assuming the solo driver: $e');
    return false;
  }
}
