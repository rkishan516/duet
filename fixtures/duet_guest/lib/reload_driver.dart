// The hot-reload driver: the Dart half of
// `cargo run -p duet-backend-macos --example hot_reload`.
//
// The third driver in this fixture, selected — like the other two — by a value
// the host writes into the shared store before the guest starts (see
// `lib/guest_support.dart`). It is the only one that calls `runApp`, and that
// is the whole point of it.
//
// # Why this one renders when the others deliberately do not
//
// `solo_driver.dart` and `duet_driver.dart` are headless because their claims
// are about the *transport*: a value crossing a platform channel needs no
// pixels. This driver's claim is about a **rendered frame**, so it needs a
// widget tree:
//
//   host edits kReloadMarker on disk
//     -> frontend_server recompiles just this library
//     -> reloadSources patches the running isolate
//     -> ext.flutter.reassemble marks the tree dirty
//     -> build() runs and puts the NEW marker into the frame
//     -> addPostFrameCallback fires, AFTER that frame was produced
//     -> the marker is written to the shared store, where Rust reads it
//
// The clock the host starts at its `fs::write` stops when that value lands in
// the Rust store, so the number it reports spans the entire chain above. Same
// technique as Spike C (`spikes/spike-c-macos/FINDINGS.md`, "Measurement
// mechanism"), with one difference stated plainly: Spike C stopped at a
// bespoke platform-channel handler, and this goes the rest of the way through
// `duet-protocol` and the runtime's core thread into the store itself. So this
// number should be expected to be a little *larger* than Spike C's, not
// smaller.
//
// # The two things that prove this is reload and not restart
//
// A hot **restart** re-runs `main()`, which rebuilds the whole widget tree and
// constructs a fresh `State`. A hot **reload** keeps both and only replaces
// code. So:
//
// - [_nonce] is assigned once, in `initState`, which a reload never re-runs.
//   If it ever changes across a reload, the isolate was recreated.
// - [_frames] counts every frame this `State` has ever produced. It must climb
//   monotonically; a reset means a new `State`.
//
// Both are published to the store on every marker change, and the Rust side
// asserts on them. Spike C used a tap count for the same purpose.
import 'dart:async';

import 'package:duet/duet.dart';
import 'package:flutter/widgets.dart';

import 'guest_support.dart';

/// **The line the Rust driver rewrites on disk between reloads.**
///
/// A top-level `const String`, alone on its line, so that each edit is a
/// single-token change for the incremental compiler and the whole diff is one
/// library. It is read inside [_ReloadProbeState.build], which is what makes a
/// changed value observable only *after* a real rebuild.
///
/// Anything that reformats this file must leave the declaration's spelling
/// intact — `const`, `String`, the name, ` = `, then a single quote, all on one
/// line. The host finds it by exactly that prefix and reports a clear failure
/// rather than a wrong reload if it cannot. `test/guest_support_test.dart`
/// pins it from this side, including that the prefix occurs exactly once in
/// this file, so a comment can never be edited by mistake.
const String kReloadMarker = 'MARKER_V1';

/// Where this driver publishes what it has rendered.
///
/// Must match `REPORT_PATH` in
/// `crates/duet-backend-macos/examples/hot_reload.rs`.
const String kReloadReportPath = 'reload';

/// Runs the reload probe against an already-handshaken host.
Future<void> runReloadGuest(DuetClient duet) async {
  guestLog('reload driver starting (this one DOES call runApp)');
  runApp(_ReloadProbe(duet: duet));
}

/// The whole app: one widget whose only job is to render [kReloadMarker].
class _ReloadProbe extends StatefulWidget {
  const _ReloadProbe({required this.duet});

  final DuetClient duet;

  @override
  State<_ReloadProbe> createState() => _ReloadProbeState();
}

class _ReloadProbeState extends State<_ReloadProbe> {
  /// A value unique to this `State` instance.
  ///
  /// Assigned in [initState], which hot reload does not re-run. This surviving
  /// a reload unchanged is the crisp discriminator between reload and restart:
  /// a restart would build a new `State` and produce a different number.
  late final int _nonce;

  /// Frames this `State` has produced, ever. Must never go backwards.
  int _frames = 0;

  /// The marker value the most recent `build` actually put into the tree.
  ///
  /// Recorded in [build] rather than read in the post-frame callback on
  /// purpose. It means the value published below is provably the one that was
  /// *rendered*, not merely the one the constant happened to hold when the
  /// callback ran — which is a weaker claim, and the one a reader would
  /// rightly be suspicious of.
  String _rendered = '';

  /// The last marker published, so the store is written on change rather than
  /// on every frame.
  String _published = '';

  @override
  void initState() {
    super.initState();
    _nonce = DateTime.now().microsecondsSinceEpoch;
    guestLog('reload probe initState: nonce=$_nonce');
    _watchFrames();
  }

  /// Registers a post-frame callback that re-registers itself.
  ///
  /// `addPostFrameCallback` does **not** request a frame — it only asks to be
  /// told about the next one. So this re-registration cannot spin: frames
  /// happen when something dirties the tree, which here means
  /// `ext.flutter.reassemble` after a reload. That matters, because a driver
  /// that requested a frame every frame is exactly the `FINDINGS.md` F1
  /// backing-store storm.
  void _watchFrames() {
    WidgetsBinding.instance.addPostFrameCallback((Duration _) {
      _frames += 1;
      if (_rendered != _published) {
        _published = _rendered;
        unawaited(_publish());
      }
      _watchFrames();
    });
  }

  /// Writes what was rendered into the shared store.
  ///
  /// Never throws: this runs from a frame callback, and taking the isolate
  /// down here would destroy the evidence the host is waiting for. A failed
  /// write shows up on the host side as a report that never arrives, which its
  /// own deadline reports.
  Future<void> _publish() async {
    try {
      await widget.duet.set(
        kReloadReportPath,
        DuetMap(<String, DuetValue>{
          'marker': DuetStr(_published),
          'frames': DuetInt(_frames),
          'nonce': DuetInt(_nonce),
        }),
      );
      guestLog('published marker=$_published frames=$_frames nonce=$_nonce');
    } on Object catch (e) {
      guestLog('ERROR publishing the reload report failed: $e');
    }
  }

  @override
  Widget build(BuildContext context) {
    // The one place the marker is read. Recording it here is what makes the
    // published value a statement about a frame rather than about a constant.
    _rendered = kReloadMarker;
    return Directionality(
      textDirection: TextDirection.ltr,
      child: ColoredBox(
        color: const Color(0xFF101820),
        child: Center(
          child: Text(
            kReloadMarker,
            style: const TextStyle(color: Color(0xFFEEEEEE), fontSize: 32),
          ),
        ),
      ),
    );
  }
}
