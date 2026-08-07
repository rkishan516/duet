/// The guest client: `window.__duet`, in Dart.
library;

import 'duet_error.dart';
import 'duet_message.dart';
import 'duet_path.dart';
import 'duet_transport.dart';
import 'duet_value.dart';

/// What [DuetClient.subscribe] hands back.
///
/// A result type of its own rather than the raw [DuetSubscribedResponse],
/// which carries *two* ids — the request's and the subscription's. Only the
/// second one matters to a caller, and a caller that reached for `.id` on the
/// response would get the wrong one with no type error to stop it.
class DuetSubscription {
  /// Creates a subscription handle.
  const DuetSubscription({required this.id, required this.snapshot});

  /// Pass this to [DuetClient.unsubscribe] to cancel.
  final int id;

  /// The watched path's value at subscription time. `null` means the path does
  /// not exist — distinct from [DuetNull], which means it exists and holds
  /// null.
  final DuetValue? snapshot;

  @override
  String toString() => 'Subscription($id, snapshot: $snapshot)';
}

/// What [DuetClient.invoke] hands back: the two ways a command that **ran** can
/// end.
///
/// # Why this is a result and not two exceptions
///
/// The protocol draws its line between *the command ran* and *the host would not
/// run it*, and this type draws the same one. `returned` and `raised` are both
/// answers from a body that executed, so they are **data**; `failed` — no such
/// command, arguments that did not decode, a body that panicked — means the call
/// never happened, so it stays a thrown [DuetFailure] alongside every other host
/// refusal this client throws.
///
/// This is the [DuetReading] argument applied to a narrower question. `DuetReading`
/// has four arms because the host has four states a path can be in and the wire
/// spends a distinct spelling on each; collapsing any two would delete a
/// distinction the protocol pays for. An `invoke` that ran has exactly two, and
/// the wire spends `returned` and `raised` on them. Where the two types differ is
/// in what they do with *failure*: a `DuetReading` is also produced by a push,
/// which has no call stack to throw into, so `get` may not throw either. An
/// `invoke` is only ever a reply to a call this guest made, so there is a stack,
/// and a refusal is free to use it.
///
/// # What a caller must do with it
///
/// `switch` over it. The hierarchy is `sealed`, so a caller that handles only
/// [DuetReturned] fails to compile rather than silently treating a command's
/// error as a success — which is the failure this type exists to make
/// impossible, and which an `invoke` returning a bare [DuetValue] would invite.
sealed class DuetInvocation {
  const DuetInvocation();
}

/// The command ran and returned `Ok`.
final class DuetReturned extends DuetInvocation {
  /// Wraps what the command returned.
  const DuetReturned(this.value);

  /// What the command returned. A command with no result answers [DuetNull].
  final DuetValue value;

  @override
  bool operator ==(Object other) => other is DuetReturned && other.value == value;

  @override
  int get hashCode => Object.hash('returned', value);

  @override
  String toString() => 'Returned($value)';
}

/// The command ran and returned `Err` — a **domain** outcome.
///
/// Carries the error as a value, not as prose, so an application can decode it
/// back into its own error type. That is the entire reason this is not a
/// [DuetFailure]: flattening a typed error into a sentence is not reversible.
final class DuetRaised extends DuetInvocation {
  /// Wraps the error the command returned.
  const DuetRaised(this.error);

  /// The error the command returned, tagged like any other value.
  final DuetValue error;

  @override
  bool operator ==(Object other) => other is DuetRaised && other.error == error;

  @override
  int get hashCode => Object.hash('raised', error);

  @override
  String toString() => 'Raised($error)';
}

/// A guest's handle on the host's shared state.
///
/// Mirrors the JavaScript guest's `window.__duet`
/// (crates/duet-webview/src/bootstrap.rs), over any [DuetTransport] rather
/// than over `wry`'s IPC specifically.
///
/// One instance owns one id sequence, so one client per transport.
class DuetClient {
  /// Wraps [transport].
  ///
  /// The transport is injected rather than constructed here: it is the only
  /// thing standing between this class and a Flutter dependency, and taking it
  /// as a parameter is what lets a test drive the whole protocol with a fake.
  DuetClient(this.transport);

  /// The transport this client speaks over.
  final DuetTransport transport;

  /// Monotonic, per the contract on `RequestId`
  /// (crates/duet-protocol/src/message.rs). **Not** used to correlate replies
  /// — see [_call].
  int _nextId = 1;

  /// The analogue of `window.__duet.onPush`: unsolicited host-to-guest traffic.
  ///
  /// Assigning this is not enough on its own; nothing arrives until [start]
  /// installs the handler on the transport.
  void Function(DuetNotification note)? onPush;

  /// Starts listening for pushes.
  ///
  /// Nothing arrives until this runs — the same silent-failure shape as a
  /// webview guest that never defines `window.__duet.onPush`.
  void start() => transport.onPush = _handleHostMessage;

  /// Stops listening for pushes. Safe to call even if [start] never ran.
  void stop() => transport.onPush = null;

  /// Reads the value at [path]. `null` means the path does not exist, which is
  /// distinct from a path that exists and holds [DuetNull].
  ///
  /// Throws [DuetCodecException] if [path] is not a legal path — before
  /// anything is sent, so a typo costs no round trip. Throws [DuetFailure] if
  /// the host refuses, and [DuetTransportException] if the exchange never
  /// reached the protocol at all.
  Future<DuetValue?> get(String path) async {
    final DuetResponse reply = await _call(
      DuetGetRequest(id: _nextId++, path: DuetPath.parse(path)),
    );
    return _expect<DuetValueResponse>(reply, 'value').value;
  }

  /// Writes [value] at [path].
  Future<void> set(String path, DuetValue value) async {
    final DuetResponse reply = await _call(
      DuetSetRequest(id: _nextId++, path: DuetPath.parse(path), value: value),
    );
    _expect<DuetDoneResponse>(reply, 'done');
  }

  /// Starts watching [path], returning the host's snapshot and the handle to
  /// cancel with.
  ///
  /// The host allocates the `SubscriberId`; this request cannot name one
  /// (crates/duet-protocol/src/message.rs).
  Future<DuetSubscription> subscribe(String path) async {
    final DuetResponse reply = await _call(
      DuetSubscribeRequest(id: _nextId++, path: DuetPath.parse(path)),
    );
    final DuetSubscribedResponse subscribed =
        _expect<DuetSubscribedResponse>(reply, 'subscribed');
    return DuetSubscription(
      id: subscribed.subscription,
      snapshot: subscribed.snapshot,
    );
  }

  /// Stops watching a subscription returned by [subscribe].
  Future<void> unsubscribe(int subscription) async {
    final DuetResponse reply = await _call(
      DuetUnsubscribeRequest(id: _nextId++, subscription: subscription),
    );
    _expect<DuetDoneResponse>(reply, 'done');
  }

  /// Runs the host command named [command] with [args].
  ///
  /// Returns a [DuetInvocation] — [DuetReturned] or [DuetRaised] — for a
  /// command that **ran**. Throws [DuetFailure] if the host **refused** to run
  /// it: there is no command by that name for this surface, the arguments did
  /// not decode, or the body panicked. See [DuetInvocation] for why that split
  /// falls where it does.
  ///
  /// [args] is a plain map because a command's parameter list is a struct's
  /// field list; it is encoded as a tagged map with its keys sorted, so the
  /// request bytes are canonical whatever order the caller built it in. A
  /// command that takes nothing is called with no second argument.
  ///
  /// Throws [DuetTransportException] if the exchange never reached the protocol,
  /// or if the host answered something that is neither a `returned` nor a
  /// `raised`.
  Future<DuetInvocation> invoke(
    String command, [
    Map<String, DuetValue> args = const <String, DuetValue>{},
  ]) async {
    final DuetResponse reply = await _call(
      DuetInvokeRequest(id: _nextId++, command: command, args: args),
    );
    return switch (reply) {
      DuetReturnedResponse(:final DuetValue value) => DuetReturned(value),
      DuetRaisedResponse(:final DuetValue error) => DuetRaised(error),
      // A `failed` never reaches here — `_call` has already thrown it — so
      // this arm is a host that answered an `invoke` with a `done` or a
      // `value`. That is a protocol violation rather than an outcome, and it
      // gets the same treatment `_expect` gives every other mismatched reply.
      _ => throw DuetTransportException(
          'expected a "returned" or "raised" response to invoke '
          '"${echoBounded(command)}", got $reply',
        ),
    };
  }

  /// Sends one request and returns the response that answers it.
  ///
  /// There is no pending-request map here, unlike the JavaScript client
  /// (crates/duet-webview/src/bootstrap.rs): [DuetTransport.send] returns a
  /// future bound to *this* message's reply, so the transport does the
  /// correlating. The `id` still travels because `duet_protocol::decode_request`
  /// requires it, and because the webview guest — which has no per-message
  /// reply channel at all — correlates by nothing else.
  ///
  /// The echoed id is still checked. A transport that mis-correlated would
  /// otherwise hand this client another request's answer, and every subsequent
  /// call would be answered one reply out of step, silently.
  Future<DuetResponse> _call(DuetRequest request) async {
    final String? text = await transport.send(request.toWireText());
    // A transport with no host listening completes with null rather than
    // throwing. Treated as a transport failure, not as silence: left
    // unhandled, `text!` would throw a "null check operator used on a null
    // value" that names neither the channel nor the request it answered.
    if (text == null) {
      throw DuetTransportException(
        'no host is listening (null reply to request ${request.id})',
      );
    }

    final DuetResponse reply = DuetResponse.fromWireText(text);
    if (reply.id != request.id) {
      // A `failed` naming an id this call did not send is the host saying it
      // could not read the id of the request it is refusing — see
      // `RequestId::UNCORRELATED` in crates/duet-protocol/src/message.rs. A
      // lone UTF-16 surrogate anywhere in the message produces exactly this.
      // Its `message` is the only account of *why*, so it travels with the
      // error: reporting the id mismatch alone would replace a diagnosis
      // ("malformed JSON: lone surrogate at line 1 column 34") with a
      // correlation complaint, which is the wrong half of the story.
      final String detail = reply is DuetFailedResponse
          ? ', and refused it: ${reply.message}'
          : '';
      throw DuetTransportException(
        'the host answered request ${reply.id} on the reply to request '
        '${request.id}$detail',
      );
    }
    if (reply is DuetFailedResponse) {
      throw DuetFailure(request.id, reply.message);
    }
    return reply;
  }

  /// Narrows a reply to the shape the call site asked for.
  ///
  /// A mismatch fails here, where the caller can name what it wanted, rather
  /// than surfacing later as a confusing cast error somewhere downstream.
  T _expect<T extends DuetResponse>(DuetResponse reply, String want) {
    if (reply is! T) {
      throw DuetTransportException(
        'expected a "$want" response, got $reply',
      );
    }
    return reply;
  }

  /// Handles every unsolicited host-to-guest message.
  ///
  /// Responses never arrive here — they come back as the reply to
  /// [DuetTransport.send].
  ///
  /// **Total against malformed input by construction.** The decode runs under
  /// `try`/`on DuetException`, so no shape of bad data — wrong types, missing
  /// fields, a non-object top level, unbounded nesting — can throw out of this
  /// method and take the isolate down. A push is fire-and-forget from the
  /// host's side: there is no request id to fail against, so the only sound
  /// response to a malformed push is to drop it.
  ///
  /// [onPush] is deliberately called *outside* the `try`. Swallowing an
  /// exception thrown by the application's own handler would hide a bug in
  /// code this package does not own, in the one place a developer would never
  /// think to look.
  void _handleHostMessage(String message) {
    final DuetPush push;
    try {
      push = DuetPush.fromWireText(message);
    } on DuetException catch (_) {
      return;
    }
    if (push is DuetNotificationPush) {
      onPush?.call(push.notification);
    }
  }
}
