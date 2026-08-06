// The fixture's own logic, and only that.
//
// The client this fixture used to carry is gone. `packages/duet` owns the wire
// format — values, paths, the envelope, `DuetClient` — and tests it against the
// cross-language golden corpus under plain `dart test`; `packages/duet_flutter`
// owns the platform channel and tests that seam. Everything this fixture's test
// suite used to assert about those two now lives there, once.
//
// What is left is the handshake in `lib/guest_support.dart`, which both drivers
// run before they do anything and which no package knows about. It is also the
// one piece of fixture logic with a real contract: there is no readiness signal
// on `duet/rpc`, so this loop is the only thing standing between a guest and a
// silent hang against a host that never registered its handler.
//
// The three macOS examples are the end-to-end proof of the drivers themselves;
// they are not reproducible under `flutter test`, which has no Rust host.
import 'package:duet/duet.dart';
import 'package:duet_guest/guest_support.dart';
import 'package:flutter_test/flutter_test.dart';

/// A transport whose reply to each request the test supplies.
class _FakeTransport implements DuetTransport {
  _FakeTransport(this._reply);

  final Future<String?> Function(String request) _reply;

  /// Every request the client put on the wire, in order.
  final List<String> sent = <String>[];

  @override
  Future<String?> send(String request) {
    sent.add(request);
    return _reply(request);
  }

  @override
  set onPush(void Function(String message)? handler) {}
}

/// The reply a conforming host gives to a `get` of a path it never seeded.
String _absent(String request) =>
    DuetValueResponse(id: DuetRequest.fromWireText(request).id, value: null)
        .toWireText();

void main() {
  test('awaitHost returns as soon as the host answers', () async {
    // The path does not exist, so the host answers `value: null` — a perfectly
    // good answer, not a failure. That is what makes this handshake harmless
    // against a host that seeded nothing: it proves a handler is listening
    // without depending on anything being in the store.
    final _FakeTransport transport = _FakeTransport(
      (String request) async => _absent(request),
    );

    expect(await awaitHost(DuetClient(transport)), isTrue);
    expect(transport.sent.length, 1, reason: 'one answer is enough');
  });

  test('the handshake request is a get of kModePath', () async {
    // Load-bearing, not incidental. The one request the handshake must make
    // anyway is the one whose answer selects the driver, so `lib/main.dart`
    // pays for no second round trip.
    final _FakeTransport transport = _FakeTransport(
      (String request) async => _absent(request),
    );
    await awaitHost(DuetClient(transport));

    final DuetRequest sent = DuetRequest.fromWireText(transport.sent.single);
    expect(sent, isA<DuetGetRequest>());
    expect((sent as DuetGetRequest).path, DuetPath.parse(kModePath));
  });

  test('awaitHost retries until the host registers its handler', () async {
    // The failure this loop exists for. A `BasicMessageChannel` with no handler
    // on the other side completes with `null` rather than throwing, which
    // `DuetClient` reports as a `DuetTransportException` — so "the host has not
    // booted yet" and "the host is gone" look identical, and the only sound
    // response is to keep asking until the deadline.
    int attempts = 0;
    final _FakeTransport transport = _FakeTransport((String request) async {
      attempts += 1;
      return attempts < 4 ? null : _absent(request);
    });

    expect(await awaitHost(DuetClient(transport)), isTrue);
    expect(attempts, 4);
  });

  test('the mode contract matches the Rust side', () {
    // `two_guests.rs` seeds MODE_PATH with DUET_MODE to select the two-guest
    // driver; `flutter_state.rs` seeds nothing, and an absent value must keep
    // the solo behaviour. Nothing links the two sides at build time, so both
    // pin the literals.
    expect(kModePath, 'mode');
    expect(kDuetMode, 'duet');
    // A deadline shorter than one attempt would give up before ever asking.
    expect(kHandshakeInterval, lessThan(kHandshakeTimeout));
  });
}
