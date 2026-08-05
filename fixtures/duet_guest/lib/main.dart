// Headless duet guest: NO runApp, NO Ticker, NO widget tree.
//
// This is a driver, not a library — it is what a later task's Rust example
// (`cargo run -p duet-backend-macos --example flutter_state`) boots inside a
// `FlutterEngine` to exercise [DuetClient] against the real host. It is not
// exercised by `flutter test` (see test/duet_client_test.dart and
// test/rust_goldens_test.dart for that); it only runs embedded, or via
// `flutter run -d macos` for a human to watch the debug console.
//
// Modeled on the scratch probe app's `lib/main.dart`, which measured this
// exact sequence working against a real engine: 12 requests served, a `set`
// visible to Rust, and a push observed by this guest.
import 'package:flutter/widgets.dart';

import 'duet_client.dart';
import 'duet_value.dart';

void _log(String s) => debugPrint('[duet_guest] $s');

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  _log('dart main() running (headless: no runApp)');

  final DuetClient duet = DuetClient()..start();
  final List<DuetNotification> pushes = <DuetNotification>[];
  duet.onPush = (DuetNotification n) {
    pushes.add(n);
    _log('PUSH #${pushes.length} path=${n.path} value=${n.value}');
  };

  // Gives the host time to register its message handler on kDuetChannel
  // before the first request goes out — there is no other signal that the
  // Rust side is ready.
  await Future<void>.delayed(const Duration(milliseconds: 300));

  try {
    await duet.set('counter', const DuetInt(42));
    _log('set counter = Int(42) OK');
    _log('get counter -> ${await duet.get('counter')}');
    final DuetSubscription sub = await duet.subscribe('counter');
    _log('subscribed id=${sub.id} snapshot=${sub.snapshot}');
  } on Object catch (e) {
    _log('ERROR $e');
  }

  // Hostile / malformed text sent RAW over the real channel, bypassing
  // DuetClient's own encoding, to prove the host's rejection path is total
  // even when the guest is not this client at all.
  for (final String bad in <String>[
    '',
    'not json',
    '42',
    '{}',
    '{"kind":"nope","id":"1"}',
    '{"kind":"get","id":"1","path":"a.[0]"}',
    '{"kind":"get","id":1,"path":"a"}',
    '{"kind":"set","id":"1","path":"a","value":{"t":"i","v":42}}',
  ]) {
    final String? reply = await kDuetChannel.send(bad);
    _log('HOSTILE ${bad.length} bytes -> ${reply?.length} byte reply: $reply');
  }
  final String huge = '{"kind":"get","id":"1","path":"${'a' * 1000000}]"}';
  final String? hugeReply = await kDuetChannel.send(huge);
  _log('HOSTILE ${huge.length} bytes -> ${hugeReply?.length} byte reply: $hugeReply');

  await Future<void>.delayed(const Duration(seconds: 3));
  _log('FINAL pushes=${pushes.length}');
}
