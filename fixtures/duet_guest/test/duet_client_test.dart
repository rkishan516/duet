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

  tearDown(() {
    messenger.setMockMessageHandler(kDuetChannel.name, null);
  });

  test('tagged encoding matches duet-codec goldens', () {
    // Pins Dart's `toJson()` byte-for-byte against
    // crates/duet-codec/src/value.rs's `encode_value` test fixtures
    // (`encodes_each_variant_with_its_tag`). If either side's tag or field
    // name drifts, the two guests silently stop agreeing on the wire shape.
    expect(jsonEncode(const DuetNull().toJson()), '{"t":"n"}');
    expect(jsonEncode(const DuetBool(true).toJson()), '{"t":"bool","v":true}');
    expect(jsonEncode(const DuetInt(42).toJson()), '{"t":"i","v":"42"}');
    expect(jsonEncode(const DuetFloat(1.5).toJson()), '{"t":"f","v":1.5}');
    expect(jsonEncode(const DuetStr('hi').toJson()), '{"t":"s","v":"hi"}');
    expect(
      jsonEncode(DuetBytes(utf8.encode('foo')).toJson()),
      '{"t":"b","v":"Zm9v"}',
    );
    expect(
      jsonEncode(const DuetList(<DuetValue>[DuetInt(1)]).toJson()),
      '{"t":"l","v":[{"t":"i","v":"1"}]}',
    );
    expect(
      jsonEncode(const DuetMap(<String, DuetValue>{'k': DuetBool(false)}).toJson()),
      '{"t":"m","v":{"k":{"t":"bool","v":false}}}',
    );
  });

  test('Int above 2^53 survives as a decimal string', () {
    // The whole reason Int travels as a string: a JSON *number* above 2^53
    // loses precision once it round-trips through an IEEE-754 double, which
    // is exactly what JSON numbers are in both Dart's `num` and JavaScript.
    // A string sidesteps that entirely, in both directions.
    const int big = 9007199254740993; // 2^53 + 1
    expect(jsonEncode(const DuetInt(big).toJson()), '{"t":"i","v":"9007199254740993"}');
    final DuetValue back =
        DuetValue.fromJson(jsonDecode('{"t":"i","v":"9007199254740993"}'));
    expect(back, const DuetInt(big));
  });

  test('a JSON-number Int is rejected, matching Rust', () {
    // Wire rule: Int MUST travel as a decimal string, never a JSON number.
    // `duet-codec/src/value.rs::decode_int` requires `payload.as_str()` and
    // fails immediately if the payload is any other JSON type — a `42` here
    // is not "the same value, less strictly typed", it is a different,
    // rejected shape. If Dart accepted a JSON-number payload, a host that
    // (incorrectly) emitted one would round-trip silently through this
    // client while a Rust guest talking to the same host would reject it —
    // the two guests would disagree about what "42" even means once it
    // crosses 2^53, since a JSON number cannot represent every i64 exactly
    // but a decimal string always can.
    expect(
      () => DuetValue.fromJson(jsonDecode('{"t":"i","v":42}')),
      throwsA(isA<DuetCodecException>()),
    );
    for (final String bad in <String>[
      '{"t":"i","v":"+5"}',
      '{"t":"i","v":"007"}',
      '{"t":"i","v":"-0"}',
      '{"t":"i","v":"999999999999999999999"}',
      '{"t":"q","v":1}',
      '{"t":"i"}',
      '42',
      '{}',
    ]) {
      expect(
        () => DuetValue.fromJson(jsonDecode(bad)),
        throwsA(isA<DuetCodecException>()),
        reason: bad,
      );
    }
  });

  test('jsonEncode throws on a raw NaN, so the sentinel is mandatory', () {
    // If DuetFloat.toJson() ever "simplified" to just emitting `value`
    // directly, this is why it would break: dart:convert's jsonEncode
    // refuses to serialize NaN/Infinity at all, so the string-sentinel
    // conversion in DuetFloat.toJson() is not optional polish, it is the
    // only reason a non-finite float can be sent at all.
    expect(() => jsonEncode(<String, Object?>{'v': double.nan}),
        throwsA(isA<JsonUnsupportedObjectError>()));
    expect(jsonEncode(const DuetFloat(double.nan).toJson()), '{"t":"f","v":"NaN"}');
    expect(jsonEncode(const DuetFloat(double.infinity).toJson()),
        '{"t":"f","v":"Infinity"}');
    final DuetValue nan = DuetValue.fromJson(jsonDecode('{"t":"f","v":"NaN"}'));
    expect((nan as DuetFloat).value.isNaN, isTrue);
  });

  test('get/set/subscribe round-trip over the channel', () async {
    final List<String> seen = <String>[];
    messenger.setMockMessageHandler(kDuetChannel.name, (ByteData? message) async {
      final String text = _codec.decodeMessage(message)!;
      seen.add(text);
      final Map<String, Object?> req = jsonDecode(text) as Map<String, Object?>;
      final String id = req['id']! as String;
      return switch (req['kind']) {
        'set' => _codec.encodeMessage('{"kind":"done","id":"$id"}'),
        'get' => _codec.encodeMessage(
            '{"kind":"value","id":"$id","value":{"t":"i","v":"42"}}'),
        'subscribe' => _codec.encodeMessage(
            '{"kind":"subscribed","id":"$id","subscription":"7","snapshot":{"t":"i","v":"42"}}'),
        _ => _codec.encodeMessage('{"kind":"failed","id":"$id","message":"nope"}'),
      };
    });

    final DuetClient duet = DuetClient();
    await duet.set('counter', const DuetInt(42));
    expect(await duet.get('counter'), const DuetInt(42));
    final DuetSubscription sub = await duet.subscribe('counter');
    expect(sub.id, 7);
    expect(sub.snapshot, const DuetInt(42));

    expect(seen[0], '{"kind":"set","path":"counter","value":{"t":"i","v":"42"},"id":"1"}');
    expect(seen[1], '{"kind":"get","path":"counter","id":"2"}');
    expect(seen[2], '{"kind":"subscribe","path":"counter","id":"3"}');
  });

  test('replies are correlated by the transport, not by the id', () async {
    // The host answers the SECOND request first: each reply carries its own
    // request's id, but they complete out of order. If DuetClient kept a
    // pending-id map (the way the JS client in bootstrap.rs must, since it
    // has no per-message reply channel), this would still pass — the point
    // is that it passes with NO such map, because BasicMessageChannel.send
    // already binds its Future to that specific message's reply
    // (platform_channel.dart:240-242). A regression that added a pending-id
    // map back in would not be caught by any *other* test here.
    messenger.setMockMessageHandler(kDuetChannel.name, (ByteData? message) async {
      final Map<String, Object?> req =
          jsonDecode(_codec.decodeMessage(message)!) as Map<String, Object?>;
      final String id = req['id']! as String;
      final String path = req['path']! as String;
      await Future<void>.delayed(Duration(milliseconds: path == 'slow' ? 60 : 5));
      return _codec.encodeMessage(
          '{"kind":"value","id":"$id","value":{"t":"s","v":"$path"}}');
    });

    final DuetClient duet = DuetClient();
    final Future<DuetValue?> slow = duet.get('slow');
    final Future<DuetValue?> fast = duet.get('fast');
    expect(await fast, const DuetStr('fast'));
    expect(await slow, const DuetStr('slow'));
  });

  test('a failed response becomes a DuetFailure on that call only', () async {
    messenger.setMockMessageHandler(kDuetChannel.name, (ByteData? message) async {
      final Map<String, Object?> req =
          jsonDecode(_codec.decodeMessage(message)!) as Map<String, Object?>;
      return _codec.encodeMessage(
          '{"kind":"failed","id":"${req['id']}","message":"invalid path"}');
    });
    await expectLater(DuetClient().get('a.[0]'), throwsA(isA<DuetFailure>()));
  });

  test('no host handler completes with null, not MissingPluginException',
      () async {
    // BasicMessageChannel differs from MethodChannel here: with no handler
    // registered on kDuetChannel, `send` completes with null rather than
    // throwing MissingPluginException. DuetClient must treat that as a
    // transport failure explicitly — left unhandled, `text!` would throw a
    // generic "null check operator used on a null value" that names neither
    // the channel nor the request it was answering.
    await expectLater(
      DuetClient().get('counter'),
      throwsA(isA<DuetTransportException>()),
    );
  });

  group('canonical decimal strings on the reply envelope', () {
    // Wire rule: id, subscription, and subscriber all travel as CANONICAL
    // decimal strings — no leading `+`, no leading zeros. Rust enforces this
    // for subscriber/subscription via duet-codec's canonical check
    // (crates/duet-codec/src/wire.rs:43-52) and always ENCODES ids with a
    // plain `u64::to_string()`, which is itself always canonical — so a
    // conforming host never emits "007" or "+1" here. But `int.tryParse` in
    // Dart happily accepts both, silently disagreeing with what a Rust guest
    // decoding the same bytes would refuse. These tests pin the defensive
    // check that keeps this guest from being the more permissive one: a
    // buggy or hostile relay that reintroduces a non-canonical form must be
    // rejected here exactly as it would be by duet-codec's own decoder,
    // not silently accepted because `int.tryParse("007") == 7`.
    for (final String bad in <String>['007', '+1']) {
      test('a non-canonical "id" on a response ($bad) is rejected', () async {
        messenger.setMockMessageHandler(kDuetChannel.name, (ByteData? message) async {
          return _codec.encodeMessage('{"kind":"done","id":"$bad"}');
        });
        await expectLater(
          DuetClient().set('a', const DuetInt(1)),
          throwsA(isA<DuetTransportException>()),
        );
      });

      test('a non-canonical "subscription" on a subscribed response ($bad) is rejected',
          () async {
        messenger.setMockMessageHandler(kDuetChannel.name, (ByteData? message) async {
          final Map<String, Object?> req =
              jsonDecode(_codec.decodeMessage(message)!) as Map<String, Object?>;
          return _codec.encodeMessage(
              '{"kind":"subscribed","id":"${req['id']}","subscription":"$bad",'
              '"snapshot":null}');
        });
        await expectLater(
          DuetClient().subscribe('a'),
          throwsA(isA<DuetTransportException>()),
        );
      });
    }
  });

  test('an unsolicited push reaches onPush', () async {
    final DuetClient duet = DuetClient()..start();
    final List<DuetNotification> pushes = <DuetNotification>[];
    duet.onPush = pushes.add;

    await messenger.handlePlatformMessage(
      kDuetChannel.name,
      _codec.encodeMessage(
        '{"kind":"notification","notification":{"subscriber":"1",'
        '"subscription":"7","patch":{"path":"counter","value":{"t":"i","v":"99"}}}}',
      ),
      (ByteData? _) {},
    );

    expect(pushes.length, 1);
    expect(pushes.single.path, 'counter');
    expect(pushes.single.value, const DuetInt(99));
    expect(pushes.single.subscription, 7);
    duet.stop();
  });

  test('a malformed push does not throw out of the handler', () async {
    // Wire rule: the push handler must be TOTAL. A push is fire-and-forget
    // from the host's side — there is no request id to fail against, so
    // there is no sound way to report a bad push back to the host. The only
    // safe behaviour is to drop it and keep the channel's message handler
    // installed; letting an exception escape `_handleHostMessage` would
    // propagate out of the platform channel's dispatch and could take the
    // whole isolate down over a single malformed message. This list mixes
    // "not JSON at all", "JSON but the wrong shape", and non-canonical ids
    // ("007", "+1") specifically, since canonicality is exactly the kind of
    // one-off validation that is easy to add for the request/response path
    // and forget for the push path.
    final DuetClient duet = DuetClient()..start();
    int calls = 0;
    duet.onPush = (DuetNotification _) => calls++;
    for (final String bad in <String>[
      'not json',
      '42',
      '{}',
      '{"kind":"notification"}',
      '{"kind":"notification","notification":{"subscriber":"007",'
          '"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}}',
      '{"kind":"notification","notification":{"subscriber":"+1",'
          '"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}}',
      '{"kind":"notification","notification":{"subscriber":"1",'
          '"subscription":"007","patch":{"path":"a","value":{"t":"n"}}}}',
    ]) {
      await messenger.handlePlatformMessage(
          kDuetChannel.name, _codec.encodeMessage(bad), (ByteData? _) {});
    }
    expect(calls, 0);
    duet.stop();
  });
}
