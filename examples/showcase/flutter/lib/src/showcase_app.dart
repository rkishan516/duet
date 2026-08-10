/// The Flutter guest's widget tree.
///
/// A real one, on purpose. The Duet fixtures in `fixtures/duet_guest` are
/// headless drivers with no `runApp`; this is an app, so that "the Flutter guest
/// is torn down and booted again" is a claim about something a user would
/// notice, and so that `duet dev` has a widget tree to hot reload.
library;

import 'package:flutter/material.dart';

import 'guest.dart';

/// The greeting this guest publishes at `flutter.note` for the webview guest.
///
/// **This is the hot-reload knob.** Change this one line, save, and `duet dev`
/// patches the running isolate: `build` runs again, the post-frame callback
/// below republishes the new text, the webview guest's watcher receives it, and
/// the Rust host prints it — with nothing restarted and the store untouched.
/// The exact command is in `examples/showcase/README.md`.
const String kFlutterNote = 'hello from Flutter';

/// The whole app: a connected panel, or an explanation of why there is none.
class ShowcaseApp extends StatelessWidget {
  /// Creates the app around an already-connected [guest], or `null` if no host
  /// answered the handshake.
  const ShowcaseApp({required this.guest, super.key});

  /// The connected guest, or `null`.
  final ShowcaseGuest? guest;

  @override
  Widget build(BuildContext context) {
    final ShowcaseGuest? connected = guest;
    return MaterialApp(
      title: 'Duet showcase — Flutter guest',
      debugShowCheckedModeBanner: false,
      theme: ThemeData.dark(useMaterial3: true),
      home: Scaffold(
        body: connected == null
            ? const _NoHost()
            : _GuestPanel(guest: connected),
      ),
    );
  }
}

class _NoHost extends StatelessWidget {
  const _NoHost();

  @override
  Widget build(BuildContext context) => const Center(
        child: Padding(
          padding: EdgeInsets.all(24),
          child: Text(
            'No Duet host answered on the duet/rpc channel.\n\n'
            'This app is a guest: it is meant to be booted by the showcase '
            'host, not run on its own with `flutter run`.',
            textAlign: TextAlign.center,
          ),
        ),
      );
}

class _GuestPanel extends StatefulWidget {
  const _GuestPanel({required this.guest});

  final ShowcaseGuest guest;

  @override
  State<_GuestPanel> createState() => _GuestPanelState();
}

class _GuestPanelState extends State<_GuestPanel> {
  String _publishedNote = '';

  @override
  void initState() {
    super.initState();
    widget.guest.addListener(_onGuestChanged);
  }

  @override
  void dispose() {
    widget.guest.removeListener(_onGuestChanged);
    super.dispose();
  }

  void _onGuestChanged() => setState(() {});

  @override
  Widget build(BuildContext context) {
    // Republish after the frame, not during build: `build` must stay free of
    // side effects, and a post-frame callback is what turns a hot reload (which
    // rebuilds the tree) into a store write the other guest can see.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_publishedNote == kFlutterNote) {
        return;
      }
      _publishedNote = kFlutterNote;
      widget.guest.publishNote(kFlutterNote);
    });

    final ShowcaseGuest guest = widget.guest;
    return SingleChildScrollView(
      padding: const EdgeInsets.all(20),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          Text('Flutter guest', style: Theme.of(context).textTheme.headlineSmall),
          const SizedBox(height: 4),
          Text('host: ${guest.hostAct} — ${guest.hostDetail}'),
          const Divider(height: 28),
          _Row('counter (shared)', guest.counter),
          const Divider(height: 28),
          _Row('document.title', guest.title),
          _Row('document.lines', '${guest.lines.length} line(s)'),
          for (final String line in guest.lines)
            Padding(
              padding: const EdgeInsets.only(left: 16, bottom: 2),
              child: Text('• $line'),
            ),
          const Divider(height: 28),
          _Row('flutter.note (mine)', kFlutterNote),
          _Row('web.note (the peer’s)', guest.peerNote),
          const Divider(height: 28),
          _Row('returned', guest.returned),
          _Row('raised', guest.raised),
          if (guest.trouble.isNotEmpty) _Row('trouble', guest.trouble),
          const SizedBox(height: 20),
          Wrap(
            spacing: 12,
            runSpacing: 8,
            children: <Widget>[
              FilledButton(
                onPressed: () => guest.appendLine(kFlutterLine),
                child: const Text('append a line'),
              ),
              OutlinedButton(
                onPressed: () => guest.appendLine(kBlankLine),
                child: const Text('append a blank line (raises)'),
              ),
              FilledButton(
                onPressed: guest.incrementCounter,
                child: const Text('+'),
              ),
            ],
          ),
          const SizedBox(height: 20),
          const Text(
            'All three buttons call Rust #[command]s. append returns or '
            'raises a typed ComposeError; + increments one counter every '
            'window shares.',
            style: TextStyle(fontSize: 12),
          ),
          const Divider(height: 36),
          Text('Host controls', style: Theme.of(context).textTheme.titleMedium),
          const SizedBox(height: 8),
          // The same verbs the webview panel offers, spelled identically:
          // both write control.request into the store, where the playground
          // host watches and obeys. Lifecycle belongs to the host — a guest
          // can only ask. Booting spawns an additional window each time.
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: <Widget>[
              for (final (String verb, String label) in const <(String, String)>[
                ('boot_flutter', 'boot a Flutter window'),
                ('suspend_flutter', 'suspend newest Flutter'),
                ('resume_flutter', 'resume newest Flutter'),
                ('teardown_flutter', 'tear newest Flutter down'),
                ('boot_web', 'boot a WebView window'),
                ('teardown_web', 'tear newest WebView down'),
                ('host_line', 'host: append a line'),
                ('sample', 'sample memory'),
                ('quit', 'quit'),
              ])
                OutlinedButton(
                  onPressed: () => guest.requestHost(verb),
                  child: Text(label),
                ),
            ],
          ),
          const SizedBox(height: 12),
          const Text(
            'These buttons write control.request into the store; the '
            'playground host watches it and obeys. Under the scripted tour '
            'nobody is listening.',
            style: TextStyle(fontSize: 12),
          ),
        ],
      ),
    );
  }
}

class _Row extends StatelessWidget {
  const _Row(this.label, this.value);

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.symmetric(vertical: 3),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: <Widget>[
            SizedBox(
              width: 180,
              child: Text(label, style: const TextStyle(color: Colors.white54)),
            ),
            Expanded(child: Text(value.isEmpty ? '—' : value)),
          ],
        ),
      );
}
