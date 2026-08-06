/// `DuetRouter`: push ownership, id-keyed routing, the early-arrival buffer,
/// and the two ways a watcher recovers from a mirror it cannot trust.
library;

import 'dart:async';

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';
import 'package:test/test.dart';

import 'editor.dart';
import 'fake_host.dart';

DuetValue _seed() => DuetMap(<String, DuetValue>{
      'editor': DuetMap(<String, DuetValue>{
        'zoom': const DuetFloat(1),
        'mode': const DuetStr('select'),
      }),
      'count': const DuetInt(1),
    });

DuetPath _p(String s) => DuetPath.parse(s);

void main() {
  group('the push slot has exactly one owner', () {
    test('attaching over an existing owner is an error, not a silent steal',
        () {
      final FakeHost host = FakeHost(_seed());
      final DuetClient client = DuetClient(host);
      // An application that wanted raw pushes for itself.
      client.onPush = (DuetNotification _) {};

      expect(() => DuetRouter(client).attach(), throwsA(isA<StateError>()));
    });

    test('a second router cannot attach to the same client', () {
      final FakeHost host = FakeHost(_seed());
      final DuetClient client = DuetClient(host);
      DuetRouter(client).attach();

      expect(() => DuetRouter(client).attach(), throwsA(isA<StateError>()));
    });

    test('attaching the same router twice is an error', () {
      final DuetRouter router = DuetRouter(DuetClient(FakeHost(_seed())));
      router.attach();

      expect(router.attach, throwsA(isA<StateError>()));
    });

    test('detaching hands the slot back', () {
      final FakeHost host = FakeHost(_seed());
      final DuetClient client = DuetClient(host);
      final DuetRouter first = DuetRouter(client)..attach();

      first.detach();
      expect(client.onPush, isNull);
      expect(host.isListening, isFalse);

      expect(DuetRouter(client).attach, returnsNormally);
    });

    test('detaching twice is safe', () {
      final DuetRouter router = DuetRouter(DuetClient(FakeHost(_seed())))
        ..attach()
        ..detach();
      expect(router.detach, returnsNormally);
      expect(router.isAttached, isFalse);
    });

    test('watching before attaching is an error, not silence', () {
      final DuetRouter router = DuetRouter(DuetClient(FakeHost(_seed())));
      final DuetField<int> count = DuetField<int>(router, 'count', duetIntCodec);

      expect(count.watch((_) {}), throwsA(isA<StateError>()));
    });

    test('a detached router stops delivering', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);

      router.detach();
      expect(host.write('count', const DuetInt(9)), isNull);
      await router.settled();

      expect(seen, isEmpty);
    });
  });

  group('routing is keyed by subscription id', () {
    test('two subscriptions on one path move independently', () async {
      // The discriminating case for id-keying: both watchers have the *same*
      // path, so an implementation that matched on the path would update both
      // and this test would fail. The host stamps every notification with the
      // subscription it answers; that is the only correct key.
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetField<int> count = DuetField<int>(router, 'count', duetIntCodec);

      final DuetWatch<int> first = await count.watch((_) {});
      final DuetWatch<int> second = await count.watch((_) {});
      expect(host.subscriptions.keys, hasLength(2));

      final int firstId = host.subscriptions.keys.first;
      host.pushTo(firstId, _p('count'), const DuetInt(42));
      await router.settled();

      expect(first.current, const DuetPresent<int>(42));
      expect(second.current, const DuetPresent<int>(1));
    });

    test('overlapping watchers each merge the same patch their own way',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();

      final DuetWatch<Editor> struct =
          await DuetField<Editor>(router, 'editor', const EditorCodec())
              .watch((_) {});
      final DuetWatch<double> leaf =
          await DuetField<double>(router, 'editor.zoom', duetFloatCodec)
              .watch((_) {});

      // One write; two notifications; two different merges — below for the
      // struct, at for the leaf.
      expect(host.write('editor.zoom', const DuetFloat(3)), isNull);
      await router.settled();

      expect(
        struct.current,
        const DuetPresent<Editor>(Editor(zoom: 3, mode: 'select')),
      );
      expect(leaf.current, const DuetPresent<double>(3));
      // Neither needed the host's help.
      expect(host.getCount, 0);
    });

    test('a notification for an unknown id does not disturb a live watcher',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});

      host.pushTo(9999, _p('count'), const DuetInt(7));
      await router.settled();

      expect(watch.current, const DuetPresent<int>(1));
    });

    test('closing unsubscribes and stops delivery', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);

      await watch.close();
      expect(watch.isClosed, isTrue);
      expect(host.subscriptions, isEmpty);

      expect(host.write('count', const DuetInt(5)), isNull);
      await router.settled();
      expect(seen, isEmpty);

      // Idempotent.
      await watch.close();
    });
  });

  group('a push can arrive before its own subscribed reply', () {
    test('it is buffered, folded, and not delivered to a handle nobody holds',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];

      // The host registers the subscription and notifies it before the guest
      // has read the reply that names it.
      host.beforeSubscribedReply = (FakeHost h, int id) {
        h.pushTo(id, _p('count'), const DuetInt(2));
        h.pushTo(id, _p('count'), const DuetInt(3));
      };

      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);

      // Caught up on return...
      expect(watch.current, const DuetPresent<int>(3));
      // ...and the application was not called back for a handle it did not
      // hold yet.
      expect(seen, isEmpty);
      expect(host.getCount, 0);
    });

    test('a buffered push is not delivered to a different subscription',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();

      host.beforeSubscribedReply = (FakeHost h, int id) {
        // A notification for an id that will never register.
        h.pushTo(id + 500, _p('count'), const DuetInt(99));
      };
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});

      expect(watch.current, const DuetPresent<int>(1));
    });

    test('the buffer is bounded, and overflow refetches instead of dropping',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host), maxBufferedPushes: 2)
        ..attach();

      host.beforeSubscribedReply = (FakeHost h, int id) {
        // Five notifications into a buffer with room for two. The last three
        // cannot be recorded, so folding what *was* recorded would leave a
        // mirror with a hole in it.
        for (int i = 2; i <= 6; i++) {
          h.pushTo(id, _p('count'), DuetInt(i));
        }
        // The store's truth, which none of those pushes carried.
        h.root = DuetMap(<String, DuetValue>{
          ...(h.root as DuetMap).entries,
          'count': const DuetInt(77),
        });
      };

      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});
      await router.settled();

      // Not 6 (the last buffered push) and not 3 (the last one that fitted):
      // the host was asked.
      expect(host.getCount, 1);
      expect(watch.current, const DuetPresent<int>(77));
    });

    test('the id map is bounded too, and every later watcher refetches',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host), maxBufferedPushes: 1)
        ..attach();

      // Notifications for two ids that will never register: the first fills
      // the id map, the second cannot even be recorded.
      host.pushTo(700, _p('count'), const DuetInt(2));
      host.pushTo(800, _p('count'), const DuetInt(3));

      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});
      await router.settled();

      // The blunt fallback fired: this watcher had nothing to do with either
      // dropped push, and still refetched rather than risk being wrong.
      expect(host.getCount, 1);
      expect(watch.current, const DuetPresent<int>(1));
    });

    test('a zero-length buffer is legal and simply always refetches', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host), maxBufferedPushes: 0)
        ..attach();

      host.beforeSubscribedReply = (FakeHost h, int id) {
        h.pushTo(id, _p('count'), const DuetInt(2));
      };
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});
      await router.settled();

      expect(host.getCount, 1);
      expect(watch.current, const DuetPresent<int>(1));
    });

    test('a negative buffer size is refused at construction', () {
      expect(
        () => DuetRouter(DuetClient(FakeHost(_seed())), maxBufferedPushes: -1),
        throwsA(isA<ArgumentError>()),
      );
    });
  });

  group('a mirror that cannot be merged is refetched', () {
    test('a patch below an absent mirror refetches rather than inventing one',
        () async {
      final FakeHost host = FakeHost(DuetMap(<String, DuetValue>{}));
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetWatch<Editor> watch =
          await DuetField<Editor>(router, 'editor', const EditorCodec())
              .watch((_) {});
      expect(watch.current, isA<DuetAbsent<Editor>>());

      // The mirror has gone stale relative to the host: some other guest wrote
      // the whole struct, and this guest is being told only about the leaf.
      // (Injected directly, because a conforming host would have sent the
      // ancestor patch too — the point is that a router which folded this into
      // an absent mirror would fabricate a struct with one field.)
      host.root = DuetMap(<String, DuetValue>{
        'editor': const EditorCodec().encode(const Editor(zoom: 4, mode: 'pan')),
      });
      final int id = host.subscriptions.keys.single;
      host.pushTo(id, _p('editor.zoom'), const DuetFloat(4));
      await router.settled();

      expect(host.getCount, 1);
      expect(
        watch.current,
        const DuetPresent<Editor>(Editor(zoom: 4, mode: 'pan')),
      );
    });

    test('a patch naming a path that does not overlap refetches', () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});

      final int id = host.subscriptions.keys.single;
      host.pushTo(id, _p('editor.mode'), const DuetStr('pan'));
      await router.settled();

      expect(host.getCount, 1);
      expect(watch.current, const DuetPresent<int>(1));
    });

    test('a refetch that fails does not throw into the application', () async {
      final FakeHost host = FakeHost(_seed())..refuseGets = true;
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);

      final int id = host.subscriptions.keys.single;
      host.pushTo(id, _p('editor.mode'), const DuetStr('pan'));
      await router.settled();

      // The last known reading is delivered rather than an exception, and the
      // host is asked once, not once per retry.
      expect(host.getCount, 1);
      expect(seen, <DuetReading<int>>[const DuetPresent<int>(1)]);
      expect(watch.current, const DuetPresent<int>(1));
    });
  });

  group('a value the codec refuses is reported and resynced', () {
    test('a mismatch is delivered, the host is asked once, and the loop stops',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);

      // Another guest writes a string where the schema says an integer.
      expect(host.write('count', const DuetStr('lots')), isNull);
      await router.settled();

      // Reported immediately, then confirmed by the refetch...
      expect(seen, hasLength(2));
      expect(seen[0], isA<DuetMismatch<int>>());
      expect(seen[1], isA<DuetMismatch<int>>());
      expect(watch.current, isA<DuetMismatch<int>>());
      // ...and asked for exactly once. A resync that itself resynced on a
      // mismatch would spin here forever, one round trip per turn.
      expect(host.getCount, 1);
    });

    test('a mismatch caused by a stale mirror is repaired by the refetch',
        () async {
      // The recovery case, not merely the reporting one: the host holds a
      // perfectly good value and this guest's mirror does not.
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);

      final int id = host.subscriptions.keys.single;
      host.pushTo(id, _p('count'), const DuetStr('garbage'));
      await router.settled();

      expect(seen.first, isA<DuetMismatch<int>>());
      expect(seen.last, const DuetPresent<int>(1));
      expect(watch.current, const DuetPresent<int>(1));
      expect(host.getCount, 1);
    });

    test('an exception from the application callback cannot cancel the resync',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetWatch<int> watch = await DuetField<int>(
        router,
        'count',
        duetIntCodec,
      ).watch((DuetReading<int> reading) {
        if (reading is DuetMismatch<int>) throw StateError('application bug');
      });

      final int id = host.subscriptions.keys.single;
      // The push escapes as the application's own exception, which this
      // package deliberately does not swallow...
      expect(
        () => host.pushTo(id, _p('count'), const DuetStr('garbage')),
        throwsA(isA<StateError>()),
      );
      await router.settled();

      // ...and the recovery still happened, because it was scheduled before
      // the callback ran.
      expect(host.getCount, 1);
      expect(watch.current, const DuetPresent<int>(1));
    });

    test('a codec that throws becomes a mismatch, not an escaping exception',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetField<String> field =
          DuetField<String>(router, 'count', const ThrowingCodec());

      final DuetReading<String> reading = await field.get();
      expect(reading, isA<DuetMismatch<String>>());
      expect((reading as DuetMismatch<String>).reason, contains('threw'));
    });
  });

  group('a notification that overtakes a refetch wins', () {
    test('the stale answer is discarded rather than overwriting a newer one',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch((_) {});
      final int id = host.subscriptions.keys.single;

      // Hold the resync's read open...
      final Completer<void> gate = Completer<void>();
      host.holdGets = gate;
      host.pushTo(id, _p('editor.mode'), const DuetStr('pan')); // forces a resync

      // ...deliver a fresher notification inside its round trip...
      await Future<void>.delayed(Duration.zero);
      host.pushTo(id, _p('count'), const DuetInt(50));
      expect(watch.current, const DuetPresent<int>(50));

      // ...then let the read finish. Its answer is older than the push.
      gate.complete();
      await router.settled();

      expect(host.getCount, 1);
      expect(watch.current, const DuetPresent<int>(50));
    });

    test('a closed watcher ignores a refetch that was already in flight',
        () async {
      final FakeHost host = FakeHost(_seed());
      final DuetRouter router = DuetRouter(DuetClient(host))..attach();
      final List<DuetReading<int>> seen = <DuetReading<int>>[];
      final DuetWatch<int> watch =
          await DuetField<int>(router, 'count', duetIntCodec).watch(seen.add);
      final int id = host.subscriptions.keys.single;

      final Completer<void> gate = Completer<void>();
      host.holdGets = gate;
      host.pushTo(id, _p('editor.mode'), const DuetStr('pan'));

      await Future<void>.delayed(Duration.zero);
      await watch.close();
      gate.complete();
      await router.settled();

      expect(seen, isEmpty);
    });
  });
}
