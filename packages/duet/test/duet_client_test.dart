import 'dart:convert';

import 'package:duet/duet.dart';
import 'package:test/test.dart';

/// A [DuetTransport] driven by a function, standing in for a platform channel.
///
/// This is the whole reason the transport is an interface. The fixture these
/// tests came from used Flutter's `TestDefaultBinaryMessenger` and could only
/// run under `flutter test`; the same assertions run here under plain
/// `dart test`, against nine lines of fake.
class FakeTransport implements DuetTransport {
  /// Answers every request with [_reply].
  FakeTransport(this._reply);

  /// A transport with no host listening: `send` completes with null, exactly
  /// as a `BasicMessageChannel` with no registered handler does.
  FakeTransport.silent() : _reply = ((String _) async => null);

  final Future<String?> Function(String request) _reply;

  /// Every request text this transport was handed, in order.
  final List<String> sent = <String>[];

  void Function(String message)? _onPush;

  @override
  Future<String?> send(String request) {
    sent.add(request);
    return _reply(request);
  }

  @override
  set onPush(void Function(String message)? handler) => _onPush = handler;

  /// True while a client is listening for pushes.
  bool get isListening => _onPush != null;

  /// Delivers an unsolicited message, as a host would.
  void deliverPush(String message) => _onPush?.call(message);
}

/// Answers each request with the reply its `kind` calls for.
Future<String?> _echoHost(String request) async {
  final DuetRequest req = DuetRequest.fromWireText(request);
  return switch (req) {
    DuetSetRequest() => '{"kind":"done","id":"${req.id}"}',
    DuetGetRequest() =>
      '{"kind":"value","id":"${req.id}","value":{"t":"i","v":"42"}}',
    DuetSubscribeRequest() =>
      '{"kind":"subscribed","id":"${req.id}","subscription":"7",'
          '"snapshot":{"t":"i","v":"42"}}',
    DuetUnsubscribeRequest() => '{"kind":"done","id":"${req.id}"}',
    DuetInvokeRequest() => _echoInvoke(req),
  };
}

/// Answers an `invoke` all three ways the protocol allows, chosen by name.
///
/// `subtract` computes `a - b` from the **decoded arguments** rather than
/// answering a constant, and subtraction is not commutative — so a client that
/// encoded its arguments under swapped names changes the answer here. A fake
/// that returned `5` whatever it was handed would pass against a completely
/// broken argument encoder.
String _echoInvoke(DuetInvokeRequest req) {
  switch (req.command) {
    case 'subtract':
      final DuetValue? a = req.args['a'];
      final DuetValue? b = req.args['b'];
      if (a is! DuetInt || b is! DuetInt) {
        return DuetFailedResponse(
          id: req.id,
          message: 'subtract expects int arguments "a" and "b"',
        ).toWireText();
      }
      return DuetReturnedResponse(
        id: req.id,
        value: DuetInt(a.value - b.value),
      ).toWireText();
    case 'nothing':
      return DuetReturnedResponse(id: req.id, value: const DuetNull())
          .toWireText();
    case 'raise':
      return DuetRaisedResponse(
        id: req.id,
        error: const DuetMap(<String, DuetValue>{
          'code': DuetStr('insufficient_funds'),
          'short_by': DuetInt(250),
        }),
      ).toWireText();
    case 'confused':
      // A host answering an `invoke` with a reply that answers no `invoke`.
      return DuetDoneResponse(id: req.id).toWireText();
    default:
      return DuetFailedResponse(
        id: req.id,
        message:
            'no command named "${req.command}" is registered for this surface',
      ).toWireText();
  }
}

void main() {
  group('request/response', () {
    test('get, set, subscribe and unsubscribe round-trip', () async {
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport);

      await duet.set('counter', const DuetInt(42));
      expect(await duet.get('counter'), const DuetInt(42));
      final DuetSubscription sub = await duet.subscribe('counter');
      expect(sub.id, 7);
      expect(sub.snapshot, const DuetInt(42));
      await duet.unsubscribe(sub.id);

      // The exact bytes, with object keys in BYTE order rather than the order
      // the fields are written in. Rust's serde_json::Map is a BTreeMap, so
      // that is what the host emits and what the golden corpus records; Dart's
      // jsonEncode walks a Map in *insertion* order, so a client that built
      // {kind, id, path} in declaration order would produce equivalent JSON
      // that is not byte-equal and would fail every byte-exact corpus case.
      expect(transport.sent, <String>[
        '{"id":"1","kind":"set","path":"counter","value":{"t":"i","v":"42"}}',
        '{"id":"2","kind":"get","path":"counter"}',
        '{"id":"3","kind":"subscribe","path":"counter"}',
        '{"id":"4","kind":"unsubscribe","subscription":"7"}',
      ]);
    });

    test('replies are correlated by the transport, not by the id', () async {
      // The host answers the SECOND request first: each reply carries its own
      // request's id, but they complete out of order. If DuetClient kept a
      // pending-id map (the way the JS client in bootstrap.rs must, since it
      // has no per-message reply channel), this would still pass — the point
      // is that it passes with NO such map, because the transport's `send`
      // future is already bound to that specific message's reply. A regression
      // that added a pending-id map back in would not be caught by any *other*
      // test here.
      final FakeTransport transport = FakeTransport((String request) async {
        final DuetGetRequest req =
            DuetRequest.fromWireText(request) as DuetGetRequest;
        final String path = req.path.toString();
        await Future<void>.delayed(
          Duration(milliseconds: path == 'slow' ? 60 : 5),
        );
        return '{"kind":"value","id":"${req.id}","value":{"t":"s","v":"$path"}}';
      });

      final DuetClient duet = DuetClient(transport);
      final Future<DuetValue?> slow = duet.get('slow');
      final Future<DuetValue?> fast = duet.get('fast');
      expect(await fast, const DuetStr('fast'));
      expect(await slow, const DuetStr('slow'));
    });

    test('the client parses the real host bytes', () async {
      // Every string below is VERBATIM stdout from the real Rust host
      // (duet_protocol::handle_text). These are not hand-written fixtures:
      // they were captured from an actual run, so a byte-for-byte change on
      // either side of the wire — a renamed field, a re-ordered key that a
      // stricter decoder would choke on, a changed tag — fails here even if
      // every unit test above (which uses hand-written JSON) still passes.
      const List<String> replies = <String>[
        '{"id":"1","kind":"done"}',
        '{"id":"2","kind":"value","value":{"t":"i","v":"42"}}',
        '{"id":"3","kind":"subscribed","snapshot":{"t":"i","v":"42"},'
            '"subscription":"0"}',
      ];
      int n = 0;
      final DuetClient duet = DuetClient(
        FakeTransport((String _) async => replies[n++]),
      );

      await duet.set('counter', const DuetInt(42));
      expect(await duet.get('counter'), const DuetInt(42));
      final DuetSubscription sub = await duet.subscribe('counter');
      expect(sub.id, 0);
      expect(sub.snapshot, const DuetInt(42));
    });

    test('a failed response becomes a DuetFailure on that call only', () async {
      final DuetClient duet = DuetClient(
        FakeTransport((String request) async {
          final DuetRequest req = DuetRequest.fromWireText(request);
          return '{"kind":"failed","id":"${req.id}","message":"invalid path"}';
        }),
      );
      await expectLater(
        duet.get('a'),
        throwsA(
          isA<DuetFailure>()
              .having((DuetFailure e) => e.requestId, 'requestId', 1)
              .having((DuetFailure e) => e.message, 'message', 'invalid path'),
        ),
      );
      // The client is not poisoned: the next call still goes out.
      await expectLater(duet.get('b'), throwsA(isA<DuetFailure>()));
    });

    test('a response of the wrong kind names what the caller wanted', () async {
      final DuetClient duet = DuetClient(
        FakeTransport((String _) async => '{"id":"1","kind":"done"}'),
      );
      await expectLater(
        duet.get('a'),
        throwsA(
          isA<DuetTransportException>().having(
            (DuetTransportException e) => e.message,
            'message',
            contains('expected a "value" response'),
          ),
        ),
      );
    });

    test('no host listening completes with null, not an exception from below',
        () async {
      // A BasicMessageChannel differs from a MethodChannel here: with no
      // handler registered, `send` completes with null rather than throwing
      // MissingPluginException. DuetClient must treat that as a transport
      // failure explicitly — left unhandled, `text!` would throw a generic
      // "null check operator used on a null value" that names neither the
      // channel nor the request it was answering.
      await expectLater(
        DuetClient(FakeTransport.silent()).get('counter'),
        throwsA(
          isA<DuetTransportException>().having(
            (DuetTransportException e) => e.message,
            'message',
            contains('no host is listening'),
          ),
        ),
      );
    });

    test('an unparseable path fails before anything is sent', () async {
      // `a.[0]` is the corpus reject case `envelope/path/unparseable`. Parsing
      // client-side costs a round trip nothing and turns a host-side rejection
      // into an error that names the offset.
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport);
      await expectLater(
        duet.get('a.[0]'),
        throwsA(
          isA<DuetCodecException>().having(
            (DuetCodecException e) => e.reason,
            'reason',
            DuetReason.badPath,
          ),
        ),
      );
      expect(transport.sent, isEmpty);
    });
  });

  group('the wire id domain', () {
    test('an id outside it is rejected', () async {
      // Mirrors duet-protocol's `the_wire_id_domain_stops_at_i64_max`. The
      // host narrowed its decoder to i64::MAX precisely because Dart's native
      // int is 64-bit signed; this pins the guest half of that agreement, so
      // the two sides cannot drift apart silently.
      expect(
        int.tryParse('9223372036854775808'),
        isNull,
        reason: 'the reason the domain stops where it does',
      );
      expect(maxWireId, 9223372036854775807);

      Future<void> replyWith(String reply) =>
          DuetClient(FakeTransport((String _) async => reply)).get('a');

      // At the boundary the id is readable, so the mismatch against request 1
      // is what fails — proving the value itself parsed rather than being
      // rejected out of hand.
      await expectLater(
        replyWith('{"id":"9223372036854775807","kind":"value","value":null}'),
        throwsA(
          isA<DuetTransportException>().having(
            (DuetTransportException e) => e.message,
            'message',
            contains('answered request 9223372036854775807'),
          ),
        ),
      );
      for (final String tooBig in <String>[
        '9223372036854775808',
        '18446744073709551615',
        '99999999999999999999',
      ]) {
        await expectLater(
          replyWith('{"id":"$tooBig","kind":"value","value":null}'),
          throwsA(
            isA<DuetCodecException>().having(
              (DuetCodecException e) => e.reason,
              'reason',
              DuetReason.badInt,
            ),
          ),
          reason: tooBig,
        );
      }
    });

    // Wire rule: id, subscription and subscriber all travel as CANONICAL
    // decimal strings — no leading `+`, no leading zeros. Rust enforces this
    // via duet-codec's canonical check and always ENCODES ids with a plain
    // `u64::to_string()`, which is itself always canonical, so a conforming
    // host never emits "007" or "+1". But `int.tryParse` in Dart happily
    // accepts both, silently disagreeing with what a Rust guest decoding the
    // same bytes would refuse. These pin the defensive check that keeps this
    // guest from being the more permissive one: a buggy or hostile relay that
    // reintroduces a non-canonical form must be rejected here exactly as
    // duet-codec's own decoder would, not silently accepted because
    // `int.tryParse("007") == 7`.
    for (final String bad in <String>['007', '+1']) {
      test('a non-canonical response id ($bad) is rejected', () async {
        await expectLater(
          DuetClient(
            FakeTransport((String _) async => '{"kind":"done","id":"$bad"}'),
          ).set('a', const DuetInt(1)),
          throwsA(
            isA<DuetCodecException>().having(
              (DuetCodecException e) => e.reason,
              'reason',
              DuetReason.badInt,
            ),
          ),
        );
      });

      test('a non-canonical subscription id ($bad) is rejected', () async {
        await expectLater(
          DuetClient(
            FakeTransport(
              (String request) async =>
                  '{"kind":"subscribed","id":"1","subscription":"$bad",'
                  '"snapshot":null}',
            ),
          ).subscribe('a'),
          throwsA(
            isA<DuetCodecException>().having(
              (DuetCodecException e) => e.reason,
              'reason',
              DuetReason.badInt,
            ),
          ),
        );
      });
    }
  });

  group('a reply that answers no request this client sent', () {
    /// The reply the host sends when it could not read the id of the request it
    /// is refusing: `RequestId::UNCORRELATED` in
    /// crates/duet-protocol/src/message.rs. A lone UTF-16 surrogate anywhere in
    /// the message produces exactly this.
    const String uncorrelated =
        '{"kind":"failed","id":"0","message":"malformed JSON: lone leading '
        'surrogate in hex escape at line 1 column 34"}';

    test('settles the call with an error rather than hanging', () async {
      // THE regression test. A client keying pending requests by the id it
      // sent finds nothing for "0"; one that dropped the reply there would
      // leave this future unsettled forever — no error, no timeout, silence.
      //
      // `timeout` is the assertion, not a convenience: without it a client that
      // hangs would wedge the suite instead of failing it.
      final DuetClient duet =
          DuetClient(FakeTransport((String _) async => uncorrelated));
      await expectLater(
        duet.get('a').timeout(const Duration(seconds: 5)),
        throwsA(isA<DuetTransportException>()),
      );
    });

    test("carries the host's own account of what went wrong", () async {
      // The `message` is the only explanation of *why* the host refused the
      // request. Reporting the id mismatch alone would replace a diagnosis with
      // a correlation complaint — the wrong half of the story, and the half a
      // developer cannot act on.
      final DuetClient duet =
          DuetClient(FakeTransport((String _) async => uncorrelated));
      await expectLater(
        duet.get('a').timeout(const Duration(seconds: 5)),
        throwsA(
          isA<DuetTransportException>().having(
            (DuetTransportException e) => e.message,
            'message',
            allOf(contains('surrogate'), contains('answered request 0')),
          ),
        ),
      );
    });
  });

  group('unpaired UTF-16 surrogates', () {
    // A lone surrogate is not a character: it has no UTF-8 encoding at all.
    // `serde_json` refuses it, so a Rust peer can never send or receive one —
    // but `jsonDecode` accepts the escape and `utf8.encode` then substitutes
    // U+FFFD for it SILENTLY. Left unchecked, the value that reaches the host
    // is not the value that left this client, with no error anywhere.
    const String loneHigh = '\uD800';

    test('cannot be encoded, in a payload or in a map key', () {
      expect(
        () => encodeDuetJson(<String, Object?>{'v': loneHigh}),
        throwsA(
          isA<DuetCodecException>().having(
            (DuetCodecException e) => e.reason,
            'reason',
            DuetReason.badJson,
          ),
        ),
      );
      expect(
        () => encodeDuetJson(<String, Object?>{loneHigh: 'v'}),
        throwsA(isA<DuetCodecException>()),
      );
      // The corruption this prevents, stated so the reason is not lost: Dart's
      // own encoders turn a lone surrogate into a replacement character with no
      // complaint at all.
      expect(utf8.encode(loneHigh), <int>[0xEF, 0xBF, 0xBD]);
    });

    test('cannot be decoded either, matching the Rust peer', () {
      // `jsonDecode` accepts `"\ud800"` happily. A guest that accepted it here
      // would decode messages every Rust peer rejects — the divergence the
      // corpus cases `value/str/lone_high_surrogate` and friends pin.
      for (final String wire in <String>[
        r'{"t":"s","v":"\ud800"}',
        r'{"t":"s","v":"\udc00"}',
        r'{"t":"m","v":{"\ud800":{"t":"n"}}}',
      ]) {
        expect(
          () => DuetValue.fromWireText(wire),
          throwsA(
            isA<DuetCodecException>().having(
              (DuetCodecException e) => e.reason,
              'reason',
              DuetReason.badJson,
            ),
          ),
          reason: '$wire must be refused',
        );
      }
    });

    test('a well-formed pair is untouched', () {
      // The check must not refuse legitimate non-BMP text. U+1F600 is the
      // surrogate pair D83D DE00 — exactly the shape the scan has to accept.
      expect(
        DuetValue.fromWireText('{"t":"s","v":"\u{1F600}"}'),
        const DuetStr('\u{1F600}'),
      );
      expect(hasUnpairedSurrogate('\u{1F600}'), isFalse);
      expect(hasUnpairedSurrogate(loneHigh), isTrue);
    });
  });

  group('pushes', () {
    test('an unsolicited push reaches onPush', () {
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport)..start();
      final List<DuetNotification> pushes = <DuetNotification>[];
      duet.onPush = pushes.add;

      transport.deliverPush(
        '{"kind":"notification","notification":{"subscriber":"1",'
        '"subscription":"7","patch":{"path":"counter",'
        '"value":{"t":"i","v":"99"}}}}',
      );

      expect(pushes.length, 1);
      expect(pushes.single.path.toString(), 'counter');
      expect(pushes.single.value, const DuetInt(99));
      expect(pushes.single.subscriber, 1);
      expect(pushes.single.subscription, 7);
    });

    test('the client parses the real host push', () {
      // Verbatim stdout from duet_protocol::encode_push, with its keys in the
      // host's own byte order rather than the order this test would have
      // written them.
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport)..start();
      final List<DuetNotification> got = <DuetNotification>[];
      duet.onPush = got.add;

      transport.deliverPush(
        '{"kind":"notification","notification":{"patch":{"path":"counter",'
        '"value":{"t":"i","v":"99"}},"subscriber":"1","subscription":"7"}}',
      );

      expect(got.single.path.toString(), 'counter');
      expect(got.single.value, const DuetInt(99));
      expect(got.single.subscription, 7);
    });

    test('nothing arrives until start, and nothing after stop', () {
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport);
      int calls = 0;
      duet.onPush = (DuetNotification _) => calls++;
      const String push =
          '{"kind":"notification","notification":{"subscriber":"1",'
          '"subscription":"7","patch":{"path":"a","value":{"t":"n"}}}}';

      expect(transport.isListening, isFalse);
      transport.deliverPush(push);
      expect(calls, 0,
          reason: 'the same silent failure as a webview guest '
              'that never defines window.__duet.onPush');

      duet.start();
      expect(transport.isListening, isTrue);
      transport.deliverPush(push);
      expect(calls, 1);

      duet.stop();
      expect(transport.isListening, isFalse);
      transport.deliverPush(push);
      expect(calls, 1);
      // stop() is safe even when start() never ran.
      DuetClient(FakeTransport.silent()).stop();
    });

    test('a malformed push does not throw out of the handler', () {
      // Wire rule: the push handler must be TOTAL. A push is fire-and-forget
      // from the host's side — there is no request id to fail against, so
      // there is no sound way to report a bad push back. The only safe
      // behaviour is to drop it and keep listening; letting an exception
      // escape would propagate out of the transport's dispatch and could take
      // the whole isolate down over a single malformed message. This list
      // mixes "not JSON at all", "JSON but the wrong shape", non-canonical ids
      // ("007", "+1") and an unparseable path, since those are exactly the
      // validations that are easy to add for the request/response path and
      // forget for the push path.
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport)..start();
      int calls = 0;
      duet.onPush = (DuetNotification _) => calls++;

      for (final String bad in <String>[
        'not json',
        '42',
        '{}',
        '{"kind":"notification"}',
        '{"kind":"nope"}',
        '{"kind":"notification","notification":{"subscriber":"1",'
            '"subscription":"1","patch":{"path":"a"}}}',
        '{"kind":"notification","notification":{"subscriber":"007",'
            '"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}}',
        '{"kind":"notification","notification":{"subscriber":"+1",'
            '"subscription":"1","patch":{"path":"a","value":{"t":"n"}}}}',
        '{"kind":"notification","notification":{"subscriber":"1",'
            '"subscription":"007","patch":{"path":"a","value":{"t":"n"}}}}',
        '{"kind":"notification","notification":{"subscriber":"1",'
            '"subscription":"1","patch":{"path":"a.[0]","value":{"t":"n"}}}}',
        '${'[' * 100000}1${']' * 100000}',
      ]) {
        expect(
          () => transport.deliverPush(bad),
          returnsNormally,
          reason: bad.length > 60 ? '${bad.substring(0, 60)}…' : bad,
        );
      }
      expect(calls, 0);
      expect(transport.isListening, isTrue,
          reason: 'still listening after a '
              'malformed push');
    });

    test('an exception from the application handler is not swallowed', () {
      // The totality guarantee covers malformed *input*, not bugs in code this
      // package does not own. Eating those would hide them in the one place a
      // developer would never think to look.
      final FakeTransport transport = FakeTransport(_echoHost);
      DuetClient(transport)
        ..start()
        ..onPush = (DuetNotification _) => throw StateError('boom');

      expect(
        () => transport.deliverPush(
          '{"kind":"notification","notification":{"subscriber":"1",'
          '"subscription":"7","patch":{"path":"a","value":{"t":"n"}}}}',
        ),
        throwsA(isA<StateError>()),
      );
    });
  });

  group('invoke', () {
    test('sends canonical bytes and returns the exact value', () async {
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport);

      // Arguments built in an order that is NOT their canonical one, so the
      // encoder's sort is what produces the bytes below rather than the
      // caller's insertion order.
      final DuetInvocation outcome = await duet.invoke(
        'subtract',
        const <String, DuetValue>{'b': DuetInt(3), 'a': DuetInt(10)},
      );

      expect(outcome, const DuetReturned(DuetInt(7)),
          reason: '10 - 3; a client that swapped the two argument names would '
              'answer -7 here');
      expect(transport.sent, <String>[
        '{"args":{"t":"m","v":{"a":{"t":"i","v":"10"},"b":{"t":"i","v":"3"}}},'
            '"command":"subtract","id":"1","kind":"invoke"}',
      ]);
    });

    test('a command taking nothing sends an empty tagged map', () async {
      // Empty, not absent: `args` is a required field, and a host decoding
      // this message refuses it outright if it is missing — see
      // `envelope/request/invoke_without_args` in the wire corpus.
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetClient duet = DuetClient(transport);

      expect(await duet.invoke('nothing'),
          const DuetReturned(DuetNull()));
      expect(transport.sent, <String>[
        '{"args":{"t":"m","v":{}},"command":"nothing","id":"1","kind":"invoke"}',
      ]);
    });

    test('a raised error arrives typed, not as prose', () async {
      // THE distinction this whole result type exists for. The error is a
      // structured value a caller can match on — a `failed` would have handed
      // over a sentence, and a sentence does not decode.
      final FakeTransport transport = FakeTransport(_echoHost);
      final DuetInvocation outcome =
          await DuetClient(transport).invoke('raise');

      expect(
        outcome,
        const DuetRaised(
          DuetMap(<String, DuetValue>{
            'code': DuetStr('insufficient_funds'),
            'short_by': DuetInt(250),
          }),
        ),
      );
      // ...and the structure really is reachable, not merely equal to a
      // literal written the same way.
      final DuetValue error = (outcome as DuetRaised).error;
      expect((error as DuetMap).entries['short_by'], const DuetInt(250));
    });

    test('a refusal throws, so it can never be mistaken for a raise', () async {
      // `raised` means the command ran and failed; `failed` means it never
      // ran. A client that surfaced both the same way would leave a caller
      // unable to decide whether retrying is safe.
      final FakeTransport transport = FakeTransport(_echoHost);
      await expectLater(
        DuetClient(transport).invoke('nope'),
        throwsA(
          isA<DuetFailure>().having(
            (DuetFailure e) => e.message,
            'message',
            'no command named "nope" is registered for this surface',
          ),
        ),
      );
    });

    test('a reply that answers no invoke is a transport failure', () async {
      // Not a `DuetFailure`: the host did not refuse anything, it answered
      // with a kind that cannot be an answer to an `invoke` at all.
      final FakeTransport transport = FakeTransport(_echoHost);
      await expectLater(
        DuetClient(transport).invoke('confused'),
        throwsA(isA<DuetTransportException>()),
      );
    });

    test('an invocation is matched exhaustively', () {
      // The compile-time half of the claim. `DuetInvocation` is sealed, so
      // this switch expression stops compiling the moment an arm is added —
      // which is the point: a caller cannot silently treat a command's error
      // as a success. There is deliberately no wildcard arm here.
      String describe(DuetInvocation outcome) => switch (outcome) {
            DuetReturned(:final DuetValue value) => 'returned $value',
            DuetRaised(:final DuetValue error) => 'raised $error',
          };
      expect(describe(const DuetReturned(DuetInt(1))), 'returned Int(1)');
      expect(describe(const DuetRaised(DuetNull())), 'raised Null');
    });

    test('every request and response kind is matched exhaustively', () {
      // The same claim for the two envelope hierarchies, which `invoke`,
      // `returned` and `raised` have just widened. Both are sealed, so a new
      // variant added without a case here is a compile error rather than a
      // silently unhandled message.
      String requestKind(DuetRequest r) => switch (r) {
            DuetGetRequest() => 'get',
            DuetSetRequest() => 'set',
            DuetSubscribeRequest() => 'subscribe',
            DuetUnsubscribeRequest() => 'unsubscribe',
            DuetInvokeRequest() => 'invoke',
          };
      String responseKind(DuetResponse r) => switch (r) {
            DuetValueResponse() => 'value',
            DuetDoneResponse() => 'done',
            DuetSubscribedResponse() => 'subscribed',
            DuetFailedResponse() => 'failed',
            DuetReturnedResponse() => 'returned',
            DuetRaisedResponse() => 'raised',
          };

      expect(
        <String>[
          requestKind(const DuetGetRequest(id: 1, path: DuetPath.root)),
          requestKind(const DuetSetRequest(
              id: 2, path: DuetPath.root, value: DuetNull())),
          requestKind(const DuetSubscribeRequest(id: 3, path: DuetPath.root)),
          requestKind(const DuetUnsubscribeRequest(id: 4, subscription: 1)),
          requestKind(const DuetInvokeRequest(
              id: 5, command: 'c', args: <String, DuetValue>{})),
        ],
        <String>['get', 'set', 'subscribe', 'unsubscribe', 'invoke'],
      );
      expect(
        <String>[
          responseKind(const DuetValueResponse(id: 1, value: null)),
          responseKind(const DuetDoneResponse(id: 2)),
          responseKind(const DuetSubscribedResponse(
              id: 3, subscription: 1, snapshot: null)),
          responseKind(const DuetFailedResponse(id: 4, message: 'no')),
          responseKind(const DuetReturnedResponse(id: 5, value: DuetNull())),
          responseKind(const DuetRaisedResponse(id: 6, error: DuetNull())),
        ],
        <String>['value', 'done', 'subscribed', 'failed', 'returned', 'raised'],
      );
    });
  });

  test('the channel name is defined once', () {
    // The one string every guest and every host must agree on. Defined here
    // rather than retyped as a literal in each binding.
    expect(duetChannelName, 'duet/rpc');
  });
}
