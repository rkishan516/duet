/// The Flutter binding for the Duet wire protocol.
///
/// Everything about the protocol — values, paths, the message envelope, the
/// client — lives in the pure-Dart `duet` package. This package adds one thing:
/// a `DuetTransport` implemented over a `BasicMessageChannel<String>` with a
/// `StringCodec`, which is how a Flutter engine embedded in a Duet host reaches
/// that host.
///
/// ```dart
/// import 'package:duet_flutter/duet_flutter.dart';
///
/// final DuetClient duet = DuetClient(DuetFlutterTransport())..start();
/// duet.onPush = (DuetNotification note) { /* a watched path changed */ };
/// await duet.set('counter', const DuetInt(42));
/// ```
///
/// # Why this is a separate package
///
/// `package:flutter/services.dart` cannot even be *loaded* under `dart test` —
/// importing it is a compile error, not a lint, because `dart:ui` does not
/// exist off-device. Keeping the one import that needs it here is what lets the
/// entire wire format be verified against the cross-language golden corpus by
/// the plain Dart SDK, in seconds, with no Flutter install.
///
/// So this package is deliberately thin: one channel constant and one adapter
/// class. If something here grows past "adapt a channel to an interface", it
/// belongs in `duet`.
///
/// `duet` is re-exported, so a Flutter guest needs one import for the whole
/// API. Splitting the two packages is an implementation concern of this
/// repository and should not become a two-import ritual at every call site.
library;

export 'package:duet/duet.dart';

export 'src/flutter_transport.dart';
