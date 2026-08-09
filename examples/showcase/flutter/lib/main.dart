/// The Flutter guest of the Duet showcase.
///
/// This app does not own any state. It attaches to a store the Rust host owns,
/// reads and writes it through the client generated from that host's
/// `#[derive(SharedState)]`, invokes the host's `#[command]` functions, and
/// watches for the *other* guest's writes. It can be torn down and booted again
/// without the store noticing.
///
/// Run it through the showcase host, not with `flutter run`:
///
/// ```console
/// $ (cd examples/showcase/flutter && flutter build macos --debug)
/// $ cargo run -p duet-showcase
/// ```
library;

import 'package:flutter/widgets.dart';

import 'src/guest.dart';
import 'src/showcase_app.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();

  // Connect before the first frame. The panel needs somewhere to read from, and
  // a guest with no host is a state the UI has to be able to draw rather than a
  // crash — a host can legitimately not be there.
  final ShowcaseGuest? guest = await ShowcaseGuest.connect();
  runApp(ShowcaseApp(guest: guest));

  // After `runApp`, so the first frame is not waiting on a round trip to Rust.
  await guest?.start();
}
