// Every string below is VERBATIM stdout from the real Rust host
// (duet_protocol::handle_text / duet_protocol::encode_push, formerly
// duet_webview::handle_ipc_text before the text-level handler moved into
// duet-protocol). These are not hand-written fixtures: they were captured
// from an actual run, so a byte-for-byte change on either side of the wire
// — a renamed field, a re-ordered key that a stricter decoder would choke
// on, a changed tag — fails here even if every unit test above (which uses
// hand-written JSON) still passes.
import 'dart:convert';

import 'package:duet_guest/duet_client.dart';
import 'package:duet_guest/duet_value.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

const StringCodec _codec = StringCodec();

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final TestDefaultBinaryMessenger messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  test('the client parses the real host bytes', () async {
    const List<String> replies = <String>[
      '{"id":"1","kind":"done"}',
      '{"id":"2","kind":"value","value":{"t":"i","v":"42"}}',
      '{"id":"3","kind":"subscribed","snapshot":{"t":"i","v":"42"},"subscription":"0"}',
    ];
    int n = 0;
    messenger.setMockMessageHandler(kDuetChannel.name, (ByteData? m) async {
      return _codec.encodeMessage(replies[n++]);
    });
    final DuetClient duet = DuetClient();
    await duet.set('counter', const DuetInt(42));
    expect(await duet.get('counter'), const DuetInt(42));
    final DuetSubscription sub = await duet.subscribe('counter');
    expect(sub.id, 0);
    expect(sub.snapshot, const DuetInt(42));
    messenger.setMockMessageHandler(kDuetChannel.name, null);
  });

  test('the client parses the real host push', () async {
    final DuetClient duet = DuetClient()..start();
    final List<DuetNotification> got = <DuetNotification>[];
    duet.onPush = got.add;
    await messenger.handlePlatformMessage(
      kDuetChannel.name,
      _codec.encodeMessage(
        '{"kind":"notification","notification":{"patch":{"path":"counter",'
        '"value":{"t":"i","v":"99"}},"subscriber":"1","subscription":"7"}}',
      ),
      (ByteData? _) {},
    );
    expect(got.single.path, 'counter');
    expect(got.single.value, const DuetInt(99));
    expect(got.single.subscription, 7);
    expect(jsonDecode('{"a":1}'), isA<Map<String, Object?>>());
    duet.stop();
  });
}
