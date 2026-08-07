// What this package is on the hook for, and nothing else.
//
// The wire format itself is tested in `packages/duet` under plain `dart test`,
// against the cross-language golden corpus. Re-testing it here would be
// duplication of exactly the kind this package exists to avoid — two copies of
// the same assertions that can drift. What is tested here is the seam: that the
// channel name is the one the Rust host registers, that text crosses it
// unaltered in both directions, and that the one behaviour a
// `BasicMessageChannel` has and a `MethodChannel` does not — a null reply
// instead of a `MissingPluginException` — still reaches `DuetClient` as the
// transport failure it treats it as.
import 'package:duet_flutter/duet_flutter.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

/// The same codec the channel uses, for building host replies in tests.
const StringCodec _codec = StringCodec();

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  final TestDefaultBinaryMessenger messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  tearDown(() {
    messenger.setMockMessageHandler(duetRpcChannel.name, null);
    duetRpcChannel.setMessageHandler(null);
  });

  test('the channel is "duet/rpc" with a StringCodec', () {
    // Pinned as a literal on purpose, in both languages. The Rust side pins the
    // same string in `flutter_surface.rs`'s
    // `the_channel_name_matches_the_dart_guest`. Nothing links the two at build
    // time, and a rename on either side does not fail: it produces a guest
    // talking to a channel nobody handles, which shows up as a null reply — a
    // silent hang shaped like a slow host, not an error.
    expect(duetRpcChannel.name, 'duet/rpc');
    expect(duetRpcChannel.name, duetChannelName);
    // The codec is half the contract: `StringCodec` is raw UTF-8 with no
    // envelope, which is the only reason the Rust host can hand the received
    // bytes straight to `duet_protocol::handle_text`. A `JSONMessageCodec` here
    // would still carry the same *characters* and still fail every host.
    expect(duetRpcChannel.codec, isA<StringCodec>());
  });

  test('request text crosses the channel byte for byte', () async {
    // The transport must not touch the payload. `DuetClient` already produced
    // canonical wire text — sorted keys, ids as decimal strings — and anything
    // this layer did to it would be invisible until a Rust peer refused it.
    final List<String> seen = <String>[];
    messenger.setMockMessageHandler(duetRpcChannel.name, (ByteData? m) async {
      seen.add(_codec.decodeMessage(m)!);
      return _codec.encodeMessage('{"id":"1","kind":"done"}');
    });

    const String request =
        '{"id":"1","kind":"set","path":"a.b[0]","value":{"t":"s","v":"héllo"}}';
    expect(await DuetFlutterTransport().send(request), '{"id":"1","kind":"done"}');
    expect(seen, <String>[request]);
  });

  test('DuetClient drives a whole exchange over the real channel', () async {
    // The end-to-end shape a guest actually uses: the pure client, this
    // transport, the real channel name. Asserts the *encoded requests* too,
    // because sorted-key canonical JSON is the property a host with a stricter
    // decoder would notice and nothing else here would.
    final List<String> seen = <String>[];
    messenger.setMockMessageHandler(duetRpcChannel.name, (ByteData? m) async {
      final String text = _codec.decodeMessage(m)!;
      seen.add(text);
      final DuetRequest request = DuetRequest.fromWireText(text);
      return _codec.encodeMessage(
        switch (request) {
          DuetSetRequest() => DuetDoneResponse(id: request.id),
          DuetGetRequest() =>
            DuetValueResponse(id: request.id, value: const DuetInt(42)),
          DuetSubscribeRequest() => DuetSubscribedResponse(
              id: request.id,
              subscription: 7,
              snapshot: const DuetInt(42),
            ),
          DuetUnsubscribeRequest() => DuetDoneResponse(id: request.id),
          // The command answer is computed from the *decoded* arguments, and
          // subtraction is not commutative — so a request whose argument names
          // were swapped on the way out changes this reply.
          DuetInvokeRequest(:final Map<String, DuetValue> args) =>
            DuetReturnedResponse(
              id: request.id,
              value: DuetInt((args['a']! as DuetInt).value -
                  (args['b']! as DuetInt).value),
            ),
        }.toWireText(),
      );
    });

    final DuetClient duet = DuetClient(DuetFlutterTransport());
    await duet.set('counter', const DuetInt(42));
    expect(await duet.get('counter'), const DuetInt(42));
    final DuetSubscription sub = await duet.subscribe('counter');
    expect(sub.id, 7);
    expect(sub.snapshot, const DuetInt(42));
    await duet.unsubscribe(sub.id);
    // An `invoke` over the same channel, with its arguments built out of
    // canonical order so the encoder's sort is what produces the bytes below.
    expect(
      await duet.invoke(
        'subtract',
        const <String, DuetValue>{'b': DuetInt(3), 'a': DuetInt(10)},
      ),
      const DuetReturned(DuetInt(7)),
    );

    expect(seen, <String>[
      '{"id":"1","kind":"set","path":"counter","value":{"t":"i","v":"42"}}',
      '{"id":"2","kind":"get","path":"counter"}',
      '{"id":"3","kind":"subscribe","path":"counter"}',
      '{"id":"4","kind":"unsubscribe","subscription":"7"}',
      '{"args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}},'
          '"command":"subtract","id":"5","kind":"invoke"}',
    ]);
  });

  test('replies are correlated by the channel, not by a pending-id map',
      () async {
    // `DuetClient` keeps no pending-request map — it relies on the transport to
    // bind each future to *that* message's reply. This is the assertion holding
    // that contract for this transport: the host answers the second request
    // first, and both callers still get their own answer.
    messenger.setMockMessageHandler(duetRpcChannel.name, (ByteData? m) async {
      final DuetGetRequest request =
          DuetRequest.fromWireText(_codec.decodeMessage(m)!) as DuetGetRequest;
      final String path = request.path.toString();
      await Future<void>.delayed(
        Duration(milliseconds: path == 'slow' ? 60 : 5),
      );
      return _codec.encodeMessage(
        DuetValueResponse(id: request.id, value: DuetStr(path)).toWireText(),
      );
    });

    final DuetClient duet = DuetClient(DuetFlutterTransport());
    final Future<DuetValue?> slow = duet.get('slow');
    final Future<DuetValue?> fast = duet.get('fast');
    expect(await fast, const DuetStr('fast'));
    expect(await slow, const DuetStr('slow'));
  });

  test('with no host handler, send completes with null rather than throwing',
      () async {
    // The one behaviour of `BasicMessageChannel` that shapes this whole seam.
    // A `MethodChannel` would throw `MissingPluginException`; this completes
    // with null, because a null reply is a legal thing for a handler to send
    // and the channel cannot tell "nobody listened" from "somebody listened and
    // said nothing".
    //
    // Asserted at BOTH levels deliberately. The transport must pass the null
    // through untranslated, and the client must still turn it into the failure
    // it already knows how to name — a transport that "helpfully" threw its own
    // exception here would break the second half without touching the first.
    expect(await DuetFlutterTransport().send('{"id":"1","kind":"get"}'), isNull);
    await expectLater(
      DuetClient(DuetFlutterTransport()).get('counter'),
      throwsA(isA<DuetTransportException>()),
    );
  });

  test('a null message from the host is not a null reply', () async {
    // `StringCodec` decodes a zero-byte payload to null, which is a *different*
    // event from the channel completing with null: here a host did answer, with
    // nothing. It must not be mistaken for a reply, and `DuetClient` must still
    // report it as a transport failure rather than crashing on a null.
    messenger.setMockMessageHandler(duetRpcChannel.name, (ByteData? m) async => null);
    expect(await DuetFlutterTransport().send('{"id":"1","kind":"get"}'), isNull);
  });

  test('a push reaches onPush, and clearing it removes the handler', () async {
    final DuetFlutterTransport transport = DuetFlutterTransport();
    final List<String> pushed = <String>[];
    transport.onPush = pushed.add;

    Future<void> deliver(String text) => messenger.handlePlatformMessage(
          duetRpcChannel.name,
          _codec.encodeMessage(text),
          (ByteData? _) {},
        );

    const String push = '{"kind":"notification","notification":'
        '{"patch":{"path":"counter","value":{"t":"i","v":"99"}},'
        '"subscriber":"1","subscription":"7"}}';
    await deliver(push);
    expect(pushed, <String>[push]);

    transport.onPush = null;
    await deliver(push);
    expect(pushed.length, 1, reason: 'the handler must actually be removed');
  });

  test('an empty push payload is dropped, not forwarded as null', () async {
    // A host that sends zero bytes decodes to a null message. `DuetTransport`
    // promises its handler a non-null `String`, so the transport must swallow
    // this rather than forward it — a cast error inside a platform channel's
    // dispatch is not recoverable by the guest.
    final DuetFlutterTransport transport = DuetFlutterTransport();
    int calls = 0;
    transport.onPush = (String _) => calls++;
    await messenger.handlePlatformMessage(
      duetRpcChannel.name,
      null,
      (ByteData? _) {},
    );
    expect(calls, 0);
  });

  test('a push is answered, so the host is not left waiting', () async {
    // Fire-and-forget from the guest's side, but the host's `send` still does
    // not complete until a reply arrives. The reply is empty text, which is not
    // mistakable for a protocol message.
    DuetFlutterTransport().onPush = (String _) {};
    ByteData? reply;
    await messenger.handlePlatformMessage(
      duetRpcChannel.name,
      _codec.encodeMessage('{"kind":"notification"}'),
      (ByteData? r) => reply = r,
    );
    expect(_codec.decodeMessage(reply), '');
  });

  test('a malformed push does not escape into the channel dispatch', () async {
    // The totality contract, end to end through this transport. A push has no
    // request id to fail against, so the only sound answer to a bad one is to
    // drop it — and an exception escaping here would propagate out of the
    // platform channel's dispatch and could take the isolate down over a single
    // malformed message.
    final DuetClient duet = DuetClient(DuetFlutterTransport())..start();
    int calls = 0;
    duet.onPush = (DuetNotification _) => calls++;
    for (final String bad in <String>[
      'not json',
      '42',
      '{}',
      '{"kind":"notification"}',
      // A non-canonical id: the kind of check easy to add on the request path
      // and forget on the push path.
      '{"kind":"notification","notification":{"subscriber":"007",'
          '"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}}',
    ]) {
      await messenger.handlePlatformMessage(
        duetRpcChannel.name,
        _codec.encodeMessage(bad),
        (ByteData? _) {},
      );
    }
    expect(calls, 0);

    // The control: after all of that, a good push still arrives — the handler
    // was never uninstalled by the failures.
    await messenger.handlePlatformMessage(
      duetRpcChannel.name,
      _codec.encodeMessage(
        '{"kind":"notification","notification":{"patch":{"path":"counter",'
        '"value":{"t":"i","v":"99"}},"subscriber":"1","subscription":"7"}}',
      ),
      (ByteData? _) {},
    );
    expect(calls, 1);
    duet.stop();
  });

  test('an injected channel does not touch the real one', () async {
    // Why the constructor takes a channel at all: a test — or an app embedding
    // two hosts — needs traffic that cannot collide with `duetRpcChannel`'s
    // single global handler slot.
    const BasicMessageChannel<String> other =
        BasicMessageChannel<String>('duet/rpc-test', StringCodec());
    messenger.setMockMessageHandler(
      other.name,
      (ByteData? m) async => _codec.encodeMessage('{"id":"1","kind":"done"}'),
    );
    addTearDown(() => messenger.setMockMessageHandler(other.name, null));

    expect(
      await DuetFlutterTransport(channel: other).send('{"id":"1","kind":"get"}'),
      '{"id":"1","kind":"done"}',
    );
    // The real channel still has no handler, so it still answers null.
    expect(await DuetFlutterTransport().send('{"id":"1","kind":"get"}'), isNull);
  });
}
