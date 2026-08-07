// Headless duet guest entry point: NO runApp, NO Ticker, NO widget tree.
//
// This is a driver, not a library — it is what a Rust example boots inside a
// `FlutterEngine` to exercise [DuetClient] against the real Rust host. It is
// not exercised by `flutter test` — the client it drives is tested in
// `packages/duet` and `packages/duet_flutter`, and what this file does can only
// be observed against a real Rust host. It runs embedded, or via
// `flutter run -d macos` for a human to watch the debug console.
//
// All this file does is shake hands with the host and choose a driver:
//
//   lib/solo_driver.dart    -> the single-guest proof (`example flutter_state`)
//   lib/duet_driver.dart    -> the two-guest proof   (`example two_guests`)
//   lib/reload_driver.dart  -> the hot-reload proof  (`example hot_reload`)
//   lib/command_driver.dart -> the command-RPC proof (`example flutter_commands`)
//
// See `lib/guest_support.dart` for why the choice is made from a value in the
// shared store rather than from a second entry point, and why an absent or
// unrecognised value keeps the solo behaviour.
// `duet_flutter` re-exports `duet`, so `DuetClient` and `duetChannelName` come
// from the same import as `DuetFlutterTransport`. The driver files import
// `package:duet/duet.dart` directly instead: they never touch a channel, and
// the narrower import is what keeps them provably transport-agnostic.
import 'package:duet_flutter/duet_flutter.dart';
import 'package:flutter/widgets.dart';

import 'command_driver.dart';
import 'duet_driver.dart';
import 'guest_support.dart';
import 'reload_driver.dart';
import 'solo_driver.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  guestLog('dart main() running (headless: no runApp)');

  // The one place this app names a transport. Everything below it — both
  // drivers, `guest_support.dart` — sees only `DuetClient`, which is the same
  // pure-Dart class a websocket or in-memory guest would use.
  final DuetClient duet = DuetClient(DuetFlutterTransport())..start();

  if (!await awaitHost(duet)) {
    guestLog('ERROR the host never answered on $duetChannelName; giving up');
    return;
  }

  switch (await _mode(duet)) {
    case kDuetMode:
      guestLog('host mode is "$kDuetMode": running the two-guest driver');
      await runDuetGuest(duet);
    case kCommandsMode:
      guestLog('host mode is "$kCommandsMode": running the command driver');
      await runCommandGuest(duet);
    case kReloadMode:
      guestLog('host mode is "$kReloadMode": running the hot-reload driver');
      await runReloadGuest(duet);
    default:
      guestLog('host mode names no driver: running the solo driver');
      await runSoloGuest(duet);
  }
}

/// Reads [kModePath] and reports which driver the host asked for.
///
/// Total: any failure — a host that rejected the read, a value of the wrong
/// shape, a path that does not exist — falls back to the solo driver rather
/// than throwing. A guest that crashed here would take the isolate down before
/// any driver ran, which is a far worse outcome than running the wrong one:
/// the Rust side asserts on what the driver publishes and would fail loudly
/// either way.
Future<String> _mode(DuetClient duet) async {
  try {
    final DuetValue? mode = await duet.get(kModePath);
    return mode is DuetStr ? mode.value : '';
  } on Object catch (e) {
    guestLog('ERROR reading "$kModePath", assuming the solo driver: $e');
    return '';
  }
}
