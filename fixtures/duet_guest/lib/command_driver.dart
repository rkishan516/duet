// The command-RPC driver: the Dart half of
// `cargo run -p duet-backend-macos --example flutter_commands`.
//
// Every other driver in this fixture reads and writes shared *state*. This one
// asks the host to *do things*: it invokes real `#[command]` functions over the
// same `duet/rpc` platform channel and publishes what came back.
//
// # Why it exists
//
// Command RPC had been driven from Dart only against `duet-host-stdio`, over a
// pipe. That proves the protocol and this client; it says nothing about whether
// an `invoke` survives a Flutter binary-messenger channel and the host's
// handler on the platform thread — which is the transport a shipped Duet
// application actually uses, and which until this increment called
// `handle_text` and refused every command by construction.
//
// # How this driver reports back to Rust
//
// Same rule as its siblings: no side channel and no log scraping. Everything
// observed is published as one map at [kCommandReportPath] over the very
// channel under test, and the Rust example polls that path out of the shared
// store.
//
// # Stages
//
// The report's `stage` field sequences the two sides:
//
//   invoked -> every command has been called and its outcome recorded
//   done    -> the driver finished; no further writes are coming
import 'dart:async';

import 'package:duet/duet.dart';

import 'guest_support.dart';

/// The one store path this driver publishes its whole observable state at.
///
/// See `lib/solo_driver.dart`'s `kReportPath` for why a single whole-map write
/// rather than one path per field.
const String kCommandReportPath = 'dartCommands';

/// The path `bump` is asked to increment, and the amount.
///
/// Must match `COUNTER_PATH` and `BUMP_BY` in
/// `crates/duet-backend-macos/examples/flutter_commands.rs`.
const String kBumpPath = 'counter';

/// How much `bump` is asked to add.
const int kBumpBy = 7;

/// The command name this guest asks for that the host was never given.
///
/// A near-miss of a registered name, so a refusal cannot be mistaken for the
/// request being malformed.
const String kUnregistered = 'subtrac';

/// How long the driver stays alive after reporting, so the Rust side has room
/// to poll before Dart stops updating.
const Duration kObserveWindow = Duration(seconds: 5);

/// Runs the command driver until its observation window elapses.
Future<void> runCommandGuest(DuetClient duet) async {
  final Map<String, DuetValue> report = <String, DuetValue>{};

  report.addAll(await _subtract(duet));
  report.addAll(await _raise(duet));
  report.addAll(await _bump(duet));
  report.addAll(await _unregistered(duet));

  await _publish(duet, report, 'invoked');
  guestLog('every command invoked; report published');

  await Future<void>.delayed(kObserveWindow);
  await _publish(duet, report, 'done');
  guestLog('command driver finished');
}

/// `subtract(a: 10, b: 3)` must answer 7.
///
/// Subtraction rather than addition, deliberately: it is not commutative, so a
/// client whose argument encoder swapped the two keys answers -7 where 7 is
/// expected. An `add` would have agreed with a completely broken encoder.
///
/// The arguments are built in the order `b`, `a` on purpose — what reaches the
/// host is the encoder's canonical sort, not this literal's order.
Future<Map<String, DuetValue>> _subtract(DuetClient duet) async {
  try {
    final DuetInvocation outcome = await duet.invoke(
      'subtract',
      const <String, DuetValue>{'b': DuetInt(3), 'a': DuetInt(10)},
    );
    return <String, DuetValue>{
      'subtractKind': DuetStr(_kindOf(outcome)),
      'subtractValue': outcome is DuetReturned ? outcome.value : const DuetNull(),
    };
  } on DuetException catch (e) {
    return <String, DuetValue>{
      'subtractKind': DuetStr('threw'),
      'subtractValue': DuetStr(e.toString()),
    };
  }
}

/// `raise()` must answer `raised`, carrying a structured error this guest can
/// match on rather than prose.
Future<Map<String, DuetValue>> _raise(DuetClient duet) async {
  try {
    final DuetInvocation outcome = await duet.invoke('raise');
    return <String, DuetValue>{
      'raiseKind': DuetStr(_kindOf(outcome)),
      'raiseError': outcome is DuetRaised ? outcome.error : const DuetNull(),
    };
  } on DuetException catch (e) {
    return <String, DuetValue>{
      'raiseKind': DuetStr('threw'),
      'raiseError': DuetStr(e.toString()),
    };
  }
}

/// `bump(path, by)` must answer the new value, and the store must hold it.
///
/// The read-back is through an ordinary `get` on the same client, so a command
/// whose write landed at some other path — or under a camel-cased key — reports
/// nothing here.
Future<Map<String, DuetValue>> _bump(DuetClient duet) async {
  try {
    final DuetInvocation outcome = await duet.invoke(
      'bump',
      const <String, DuetValue>{'path': DuetStr(kBumpPath), 'by': DuetInt(kBumpBy)},
    );
    final DuetValue? held = await duet.get(kBumpPath);
    return <String, DuetValue>{
      'bumpKind': DuetStr(_kindOf(outcome)),
      'bumpValue': outcome is DuetReturned ? outcome.value : const DuetNull(),
      'bumpReadBack': held ?? const DuetNull(),
    };
  } on DuetException catch (e) {
    return <String, DuetValue>{
      'bumpKind': DuetStr('threw'),
      'bumpValue': DuetStr(e.toString()),
      'bumpReadBack': const DuetNull(),
    };
  }
}

/// A command this surface was never given must throw [DuetFailure] — the host
/// would not run it — and never arrive as a [DuetRaised], which would say
/// something ran and failed.
Future<Map<String, DuetValue>> _unregistered(DuetClient duet) async {
  try {
    final DuetInvocation outcome = await duet.invoke(kUnregistered);
    return <String, DuetValue>{'unknownKind': DuetStr(_kindOf(outcome))};
  } on DuetFailure catch (e) {
    return <String, DuetValue>{
      'unknownKind': const DuetStr('failed'),
      'unknownMessage': DuetStr(e.message),
    };
  } on DuetException catch (e) {
    return <String, DuetValue>{'unknownKind': DuetStr('threw: $e')};
  }
}

/// Names an invocation's arm, so the Rust side asserts on a word rather than on
/// a Dart type name that could change without the wire changing.
String _kindOf(DuetInvocation outcome) =>
    outcome is DuetReturned ? 'returned' : 'raised';

/// Writes the whole report, with `stage` set.
Future<void> _publish(
  DuetClient duet,
  Map<String, DuetValue> report,
  String stage,
) async {
  try {
    await duet.set(
      kCommandReportPath,
      DuetMap(<String, DuetValue>{...report, 'stage': DuetStr(stage)}),
    );
  } on DuetException catch (e) {
    guestLog('ERROR publishing the report at stage "$stage": $e');
  }
}
