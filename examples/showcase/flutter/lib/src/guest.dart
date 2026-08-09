/// The Flutter guest's side of the Duet conversation.
///
/// Everything below goes through the generated client in `showcase.duet.dart`.
/// There is not one hand-written path string in this file — `state.document
/// .lines` carries the literal `'document.lines'` that `duet generate` minted
/// from the Rust definition, and `commands.appendLine` carries the literal
/// `'append_line'`. That is the whole point of the generated layer: rename a
/// field in Rust and this file stops compiling, rather than silently reading a
/// key nobody writes any more.
library;

import 'dart:async';

import 'package:duet/typed.dart';
import 'package:duet_flutter/duet_flutter.dart';
import 'package:flutter/foundation.dart';

import 'showcase.duet.dart';

/// The line this guest contributes to the shared document.
const String kFlutterLine = 'Flutter: a real widget tree, sharing one store.';

/// A line that cannot be composed, so `append_line` raises rather than returns.
const String kBlankLine = '   ';

/// How long to keep knocking before deciding no host is listening.
const Duration kHandshakeTimeout = Duration(seconds: 10);

/// How long to wait between knocks.
const Duration kHandshakeInterval = Duration(milliseconds: 25);

/// Everything this guest has read, written, or been told, in one listenable.
///
/// A [ChangeNotifier] rather than a stream per field: every value here arrives
/// on a push, the UI redraws as a whole, and one notification per push keeps the
/// widget layer from having to know which paths exist.
class ShowcaseGuest extends ChangeNotifier {
  ShowcaseGuest._(this._client, this.state, this.commands);

  /// Connects to the host, or returns `null` if none answers.
  ///
  /// The handshake is a poll, and it has to be: a [BasicMessageChannel] with no
  /// handler on the other side completes with `null` rather than waiting, which
  /// `DuetClient` reports as a [DuetTransportException]. There is no readiness
  /// signal to await, so the only honest way to find the host is to knock until
  /// it answers. Only a transport failure is retried — a real refusal from a
  /// live host must not be swallowed by a retry loop.
  static Future<ShowcaseGuest?> connect() async {
    final DuetClient client = DuetClient(DuetFlutterTransport());
    final Stopwatch elapsed = Stopwatch()..start();
    while (elapsed.elapsed < kHandshakeTimeout) {
      try {
        await client.get('host.act');
        // `attach()` calls `client.start()` and takes ownership of the push
        // slot. Never also assign `client.onPush` — the router would stop
        // seeing the traffic its watchers depend on.
        final DuetRouter router = DuetRouter(client)..attach();
        return ShowcaseGuest._(
          client,
          ShowcaseClient(router),
          ShowcaseCommands(client),
        );
      } on DuetTransportException catch (_) {
        await Future<void>.delayed(kHandshakeInterval);
      }
    }
    return null;
  }

  final DuetClient _client;

  /// The typed view of the shared store.
  final ShowcaseClient state;

  /// The typed view of the host's commands.
  final ShowcaseCommands commands;

  /// The shared title, as this guest last saw it.
  String title = '';

  /// The shared document, as this guest last saw it.
  List<String> lines = const <String>[];

  /// What the *webview* guest wrote for this one to read.
  String peerNote = '';

  /// The host's running commentary.
  String hostAct = '';

  /// One line of detail about [hostAct].
  String hostDetail = '';

  /// The `returned` arm of this guest's own command calls.
  String returned = '';

  /// The `raised` arm of them.
  String raised = '';

  /// Anything that went wrong locally, for the panel to show.
  String trouble = '';

  /// Subscribes to everything this guest displays, then runs its opening moves.
  ///
  /// Watchers are armed *before* the first command, so the push produced by this
  /// guest's own `append_line` is one it is already listening for.
  Future<void> start() async {
    await _watchEverything();
    await _openingMoves();
    // Written last, and only once the rest has landed. The host treats
    // `flutter.status == "ready"` as "this guest has finished its opening
    // moves", so setting it earlier would let the host read half a story.
    await _write(() => state.flutter.status.set('ready'), 'flutter.status');
  }

  /// Arms every watcher, then feeds each one the snapshot it was created with.
  ///
  /// The second half is load-bearing and easy to miss. A callback fires only for
  /// changes *after* the subscription; the value the path already held arrives
  /// as [DuetWatch.current], synchronously, the instant `watch` returns. A guest
  /// that attaches after its peer has already written — which is exactly what
  /// happens when this guest is torn down and booted again — would otherwise
  /// never learn what the peer wrote.
  Future<void> _watchEverything() async {
    final DuetWatch<String> title = await state.document.title.watch(_onTitle);
    _onTitle(title.current);

    final DuetWatch<List<String>> lines =
        await state.document.lines.watch(_onLines);
    _onLines(lines.current);

    final DuetWatch<String> peer = await state.web.note.watch(_onPeerNote);
    _onPeerNote(peer.current);

    final DuetWatch<HostNote> host = await state.host.self.watch(_onHostNote);
    _onHostNote(host.current);
  }

  void _onTitle(DuetReading<String> reading) {
    title = _text(reading);
    notifyListeners();
  }

  void _onLines(DuetReading<List<String>> reading) {
    lines = switch (reading) {
      DuetPresent<List<String>>(:final List<String> value) => value,
      _ => const <String>[],
    };
    notifyListeners();
    // Mirrored into the store so the host — which has no screen to look at —
    // can see that this guest's watcher actually fired.
    unawaited(_write(
      () => state.flutter.sawLines.set(lines.length),
      'flutter.saw_lines',
    ));
  }

  void _onPeerNote(DuetReading<String> reading) {
    peerNote = _text(reading);
    notifyListeners();
    unawaited(_write(
      () => state.flutter.sawPeerNote.set(peerNote),
      'flutter.saw_peer_note',
    ));
  }

  void _onHostNote(DuetReading<HostNote> reading) {
    switch (reading) {
      case DuetPresent<HostNote>(:final HostNote value):
        hostAct = value.act;
        hostDetail = value.detail;
      default:
        hostAct = _text(_asText(reading));
        hostDetail = '';
    }
    notifyListeners();
  }

  /// One `append_line` that succeeds, one that is refused, and one pure command.
  ///
  /// Both arms on every run: a demo that only shows the happy path teaches the
  /// wrong shape, because `raised` is not an exception here — it is a typed
  /// value the generated client hands back.
  Future<void> _openingMoves() async {
    returned = await appendLine(kFlutterLine);
    raised = await appendLine(kBlankLine);
    notifyListeners();
  }

  /// Invokes `append_line` and records whichever arm came back.
  ///
  /// Returns the rendered outcome, and also stores it in [returned] or [raised]
  /// so the buttons in the panel update the same fields the opening moves did.
  Future<String> appendLine(String text) async {
    String rendered = '';
    try {
      final DuetOutcome<int, ComposeError> outcome =
          await commands.appendLine(text: text);
      switch (outcome) {
        case DuetOk<int, ComposeError>(:final int value):
          rendered = 'append_line(${_quote(text)}) returned $value';
          returned = rendered;
          await _write(
            () => state.flutter.returned.set(rendered),
            'flutter.returned',
          );
        case DuetErr<int, ComposeError>(:final ComposeError error):
          rendered =
              'append_line(${_quote(text)}) raised ${error.code}: ${error.detail}';
          raised = rendered;
          await _write(
            () => state.flutter.raised.set(rendered),
            'flutter.raised',
          );
        case DuetUndecodable<int, ComposeError>(raised: final bool wasRaised):
          rendered = 'append_line(${_quote(text)}) answered something the '
              'schema does not describe (raised: $wasRaised)';
          trouble = rendered;
      }
    } on DuetFailure catch (e) {
      // A refusal, not a raise: the host declined to run the command at all.
      // Distinct from `DuetErr` on purpose — this one means the two sides
      // disagree about what exists, which no amount of retrying fixes.
      rendered = 'append_line(${_quote(text)}) was refused: ${e.message}';
      trouble = rendered;
    }
    notifyListeners();
    return rendered;
  }

  /// Publishes [note] at `flutter.note` for the webview guest to read.
  ///
  /// Called from a post-frame callback, so a hot reload that changes the
  /// constant re-publishes it without anything restarting.
  Future<void> publishNote(String note) =>
      _write(() => state.flutter.note.set(note), 'flutter.note');

  Future<void> _write(Future<void> Function() write, String what) async {
    try {
      await write();
    } on DuetException catch (e) {
      // Never rethrow: half of these run from a push handler, and taking the
      // isolate down would destroy the evidence the host is about to read.
      trouble = 'writing $what failed: $e';
      notifyListeners();
    }
  }

  @override
  void dispose() {
    _client.stop();
    super.dispose();
  }
}

/// Renders a four-way reading as one line.
///
/// A read is never an exception: `present`, `none` (an explicit null), `absent`
/// (no node at all), and `mismatch` (another guest wrote a type this codec
/// refuses) are four states a UI has to be able to draw.
String _text(DuetReading<String> reading) => switch (reading) {
      DuetPresent<String>(:final String value) => value,
      DuetNone<String>() => '(null)',
      DuetAbsent<String>() => '(absent)',
      DuetMismatch<String>(:final String reason) => '(mismatch: $reason)',
    };

DuetReading<String> _asText(DuetReading<HostNote> reading) => switch (reading) {
      DuetNone<HostNote>() => const DuetNone<String>(),
      DuetMismatch<HostNote>(:final DuetValue found, :final String reason) =>
        DuetMismatch<String>(found, reason),
      _ => const DuetAbsent<String>(),
    };

String _quote(String text) => "'$text'";
