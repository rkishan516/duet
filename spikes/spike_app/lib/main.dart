import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

// Spike B: platform channel used by the Rust host to prove that a message
// sent from a background "core thread" via tao's EventLoopProxy actually
// lands on the Flutter side, and gets a reply the host can read.
const MethodChannel _spikeBChannel = MethodChannel('duet/spike_b');

// Spike C: separate channel used purely for hot-reload latency measurement.
// The host edits kReloadMarker's value on disk, drives an incremental
// recompile + reloadSources, and then waits to see the NEW marker value
// arrive over this channel - that closes the loop from "source file
// written" to "the changed UI has actually rendered a frame", which is what
// exit criterion 5 asks to be measured, not just the RPC round trip.
const MethodChannel _spikeCChannel = MethodChannel('duet/spike_c');

// THIS is the line Spike C's driver edits on disk between reload iterations
// (e.g. 'MARKER_V1' -> 'MARKER_V2' -> ...). Kept as a single top-level const
// string so each edit is a single-line, single-token diff for the
// incremental compiler to pick up.
const String kReloadMarker = 'MARKER_V1';

void main() {
  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Duet Spike B',
      theme: ThemeData(colorScheme: ColorScheme.fromSeed(seedColor: Colors.deepPurple)),
      home: const MyHomePage(title: 'Duet Spike B - Flutter side'),
    );
  }
}

class MyHomePage extends StatefulWidget {
  const MyHomePage({super.key, required this.title});

  final String title;

  @override
  State<MyHomePage> createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage>
    with SingleTickerProviderStateMixin, WidgetsBindingObserver {
  // Counts every vsync tick this widget has been asked to repaint, driven by
  // Flutter's own scheduler - NOT by anything the Rust host does. This is the
  // "Flutter keeps making independent progress" proof for exit criterion 2.
  int _frameTicks = 0;

  // Counts how many times the native 'ping' method has been invoked on
  // _spikeBChannel from Rust (via engine.binaryMessenger / FlutterMethodChannel).
  // This is the "message actually landed" proof for exit criterion 3.
  int _pingCount = 0;

  // Counts real Flutter-side tap gestures on the FloatingActionButton,
  // separately from synthetic-input-driven taps, so criterion 5 can tell
  // whether a synthetic NSEvent produced a real tap.
  int _tapCount = 0;

  late final Ticker _ticker;

  @override
  void initState() {
    super.initState();
    // duet-backend-macos F1 fix verification: the Rust host now sends real
    // AppLifecycleState transitions on flutter/lifecycle around detach and
    // attach (see crates/duet-backend-macos/src/engine.rs and FINDINGS.md).
    // This print independently confirms a transition actually arrived at
    // the framework, rather than inferring it from the absence of the
    // "Could not create the embedder backing store" retry storm.
    WidgetsBinding.instance.addObserver(this);
    _spikeBChannel.setMethodCallHandler(_handleMethodCall);
    _ticker = createTicker((_) {
      if (!mounted) return;
      setState(() {
        _frameTicks++;
      });
    });
    _ticker.start();
    // Kick off the recurring post-frame reporter (Spike C latency probe).
    SchedulerBinding.instance.addPostFrameCallback(_reportFrame);
  }

  Future<dynamic> _handleMethodCall(MethodCall call) async {
    switch (call.method) {
      case 'ping':
        final n = call.arguments;
        setState(() {
          _pingCount++;
        });
        return 'pong pingCount=$_pingCount frameTicks=$_frameTicks tapCount=$_tapCount n=$n';
      case 'query':
        return 'frameTicks=$_frameTicks pingCount=$_pingCount tapCount=$_tapCount';
      case 'increment':
        // Deterministic alternative to synthetic-input-driven taps (Spike B
        // found synthetic NSEvents unreliable in this windowless
        // environment). This is what Spike C uses to drive Dart heap state
        // to a non-zero value before reloading, to test whether reload
        // preserves it.
        setState(() {
          _tapCount++;
        });
        return _tapCount;
      default:
        return null;
    }
  }

  // Fires after every frame (re-registers itself each time, since
  // addPostFrameCallback only fires once per registration). Reports the
  // CURRENT value of kReloadMarker plus tapCount to the host. Because
  // kReloadMarker is read directly in build() below, a hot reload that
  // patches this file's compiled code changes what this method sends on
  // the very next frame - which is exactly the "edit -> pixel" signal
  // Spike C's driver waits on.
  void _reportFrame(Duration _) {
    _spikeCChannel.invokeMethod('frameMarker', <String, dynamic>{
      'marker': kReloadMarker,
      'tapCount': _tapCount,
    });
    SchedulerBinding.instance.addPostFrameCallback(_reportFrame);
  }

  void _incrementCounter() {
    setState(() {
      _tapCount++;
    });
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // ignore: avoid_print
    print('[spike_app] didChangeAppLifecycleState: $state');
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _ticker.dispose();
    _spikeBChannel.setMethodCallHandler(null);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        backgroundColor: Theme.of(context).colorScheme.inversePrimary,
        title: Text(widget.title),
      ),
      // GestureDetector fills the whole body (HitTestBehavior.opaque) so a
      // synthetic click anywhere in the window body - not just on the tiny
      // FAB - can be used to test whether synthetic input routing (exit
      // criterion 5) reaches Flutter's gesture pipeline at all.
      body: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: _incrementCounter,
        child: Center(
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              Text(
                'marker: $kReloadMarker',
                style: Theme.of(context).textTheme.headlineSmall?.copyWith(color: Colors.deepPurple),
              ),
              const SizedBox(height: 8),
              Text('frame ticks: $_frameTicks', style: Theme.of(context).textTheme.headlineSmall),
              const SizedBox(height: 8),
              Text('pings received: $_pingCount', style: Theme.of(context).textTheme.headlineSmall),
              const SizedBox(height: 8),
              Text('taps: $_tapCount', style: Theme.of(context).textTheme.headlineSmall),
              const SizedBox(height: 24),
              RotationTransition(
                turns: AlwaysStoppedAnimation((_frameTicks % 120) / 120.0),
                child: const Icon(Icons.refresh, size: 48),
              ),
            ],
          ),
        ),
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: _incrementCounter,
        tooltip: 'Increment',
        child: const Icon(Icons.add),
      ),
    );
  }
}
