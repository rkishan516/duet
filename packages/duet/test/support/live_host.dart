/// A [DuetTransport] over the real Rust host, running as a child process.
///
/// `crates/duet-host-stdio` wraps `duet_protocol::handle_text` in a process
/// that speaks newline-delimited JSON on stdin and stdout. This is the Dart
/// side of that pipe, and it is what lets `live_host_test.dart` drive the
/// **generated** client against the host rather than against a fake
/// transcribed from `crates/duet-core/src/value.rs`.
///
/// # Correlation lives here, as the transport contract requires
///
/// [DuetTransport.send] must return a future bound to *this* message's reply;
/// `DuetClient` keeps no pending map. A single pipe carries every reply and
/// every push interleaved, so this class reads the outgoing request's `id` and
/// keys a completer by it.
///
/// # Nothing here may hang
///
/// A host that wedged would otherwise leave `dart test` waiting on a completer
/// that never completes, and the run would time out with no indication of
/// which call was outstanding. Three separate guards:
///
/// - every [send] is bounded by [replyTimeout];
/// - the process exiting completes every pending call with an error naming its
///   exit code and whatever it wrote to stderr;
/// - a reply whose id matches nothing is recorded in [unmatched] rather than
///   dropped, so a test can assert the stream held no surprises.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:duet/duet.dart';

/// How long one request may wait for its reply.
///
/// Generous: every operation this host performs is microseconds of in-process
/// work, so anything approaching this is a wedge rather than slowness, and the
/// only cost of a large bound is how long a genuinely wedged run takes to fail.
const Duration replyTimeout = Duration(seconds: 10);

/// The environment variable that names the host binary.
///
/// When it is set, a binary that cannot be found or cannot be started is a
/// **failure**, never a skip: CI sets this, and a typo in it would otherwise
/// turn the entire live-host suite into silence that still exits zero.
const String hostPathVariable = 'DUET_HOST_STDIO';

/// Where the binary lands under a plain `cargo test` or `cargo build`.
///
/// Relative to this package's root, which is the working directory `dart test`
/// runs in.
const List<String> _defaultHostPaths = <String>[
  '../../target/debug/duet-host-stdio',
  '../../target/release/duet-host-stdio',
];

/// How to build the host, named in every diagnostic that cannot find it.
const String buildCommand = 'cargo build -p duet-host-stdio';

/// The host binary, or null if there is none to be found.
///
/// Throws [StateError] if [hostPathVariable] names a file that is not there —
/// see that constant for why an explicit override may not degrade to a skip.
String? locateDuetHost() {
  final String? named = Platform.environment[hostPathVariable];
  if (named != null && named.isNotEmpty) {
    if (!File(named).existsSync()) {
      throw StateError(
        '$hostPathVariable names "$named", which does not exist. An explicit '
        'override must not silently skip the conformance run. Build the host '
        'with:\n    $buildCommand',
      );
    }
    return named;
  }
  for (final String candidate in _defaultHostPaths) {
    if (File(candidate).existsSync()) return candidate;
  }
  return null;
}

/// Why the live-host tests are being skipped, or null if they can run.
String? liveHostSkipReason() {
  if (locateDuetHost() != null) return null;
  return 'the duet-host-stdio binary was not found; build it with '
      '`$buildCommand`, or set $hostPathVariable to its path';
}

/// A transport over a running `duet-host-stdio` process.
final class StdioHost implements DuetTransport {
  StdioHost._(this._process, this._schema) {
    _stdout = _process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(_receive, onDone: _closeStream);
    _stderr = _process.stderr.transform(utf8.decoder).listen(_errors.write);
    unawaited(_process.exitCode.then(_exited));
  }

  /// Starts a host seeded from the named schema fixture.
  ///
  /// Throws [StateError] if no binary can be found.
  static Future<StdioHost> start(String schema) async {
    final String? binary = locateDuetHost();
    if (binary == null) {
      throw StateError(
        'no duet-host-stdio binary; build it with `$buildCommand`',
      );
    }
    final Process process = await Process.start(
      binary,
      <String>[schema],
      // Inherit nothing: the host reads only stdin and writes only stdout, and
      // a working directory of our own keeps a relative path in a diagnostic
      // meaningful.
      workingDirectory: Directory.current.path,
    );
    return StdioHost._(process, schema);
  }

  final Process _process;
  final String _schema;
  final Map<String, Completer<String>> _pending = <String, Completer<String>>{};
  final StringBuffer _errors = StringBuffer();

  /// Every line the host sent that answered no outstanding request and was not
  /// a push.
  ///
  /// Empty in a correct run. Recorded rather than dropped because a reply that
  /// matches nothing is the shape of a correlation bug, and silence about it
  /// is how such a bug survives.
  final List<String> unmatched = <String>[];

  late final StreamSubscription<String> _stdout;
  late final StreamSubscription<String> _stderr;
  void Function(String message)? _onPush;
  bool _closed = false;
  int? _exitCode;

  @override
  set onPush(void Function(String message)? handler) => _onPush = handler;

  /// The schema fixture this host was seeded from.
  String get schema => _schema;

  @override
  Future<String?> send(String request) {
    if (_closed) {
      throw StateError('the host for "$_schema" has already stopped$_diagnostic');
    }
    final String id = _idOf(request);
    final Completer<String> reply = Completer<String>();
    if (_pending.containsKey(id)) {
      throw StateError('request id $id is already outstanding');
    }
    _pending[id] = reply;
    _process.stdin.write('$request\n');
    return reply.future.timeout(
      replyTimeout,
      onTimeout: () {
        _pending.remove(id);
        throw StateError(
          'the host for "$_schema" did not answer request $id within '
          '${replyTimeout.inSeconds}s$_diagnostic',
        );
      },
    );
  }

  /// Stops the host and releases its streams. Idempotent.
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _failPending('the host was closed by the test');
    await _process.stdin.close();
    // The host exits on end of input; kill only if it does not, so a hung host
    // fails a test rather than hanging the suite.
    await _process.exitCode.timeout(
      const Duration(seconds: 5),
      onTimeout: () {
        _process.kill(ProcessSignal.sigkill);
        return -1;
      },
    );
    await _stdout.cancel();
    await _stderr.cancel();
  }

  /// Routes one line from the host.
  void _receive(String line) {
    final Object? decoded;
    try {
      decoded = jsonDecode(line);
    } on FormatException {
      unmatched.add(line);
      return;
    }
    if (decoded is! Map<String, Object?>) {
      unmatched.add(line);
      return;
    }
    // A push is told from a reply by its envelope kind, which is the same rule
    // every other Duet transport uses. `duet_protocol::Push` has exactly one
    // arm, and a guest that guessed by the absence of an `id` would break the
    // day it gained a second.
    if (decoded['kind'] == 'notification') {
      _onPush?.call(line);
      return;
    }
    final Object? id = decoded['id'];
    final Completer<String>? waiting =
        id is String ? _pending.remove(id) : null;
    if (waiting == null) {
      unmatched.add(line);
      return;
    }
    waiting.complete(line);
  }

  /// The `id` of an outgoing request.
  String _idOf(String request) {
    final Object? decoded = jsonDecode(request);
    if (decoded is! Map<String, Object?> || decoded['id'] is! String) {
      throw StateError('cannot correlate a request with no string id: $request');
    }
    return decoded['id']! as String;
  }

  void _exited(int code) {
    _exitCode = code;
    _failPending('the host exited with code $code');
  }

  void _closeStream() {
    _failPending('the host closed its output stream');
  }

  /// Completes every outstanding call with an error, so nothing waits forever.
  void _failPending(String because) {
    if (_pending.isEmpty) return;
    final List<Completer<String>> waiting = _pending.values.toList();
    _pending.clear();
    for (final Completer<String> completer in waiting) {
      if (!completer.isCompleted) {
        completer.completeError(StateError('$because$_diagnostic'));
      }
    }
  }

  /// Whatever the host said on stderr, and how it ended.
  String get _diagnostic {
    final String errors = _errors.toString().trim();
    final String exit = _exitCode == null ? '' : ' (exit code $_exitCode)';
    return errors.isEmpty ? exit : '$exit\nhost stderr: $errors';
  }
}
