/// The hand-written schema type the typed tests share.
///
/// Stands in for what increment 4 will generate: a struct, a codec that is
/// total and answers `null` rather than throwing, and nothing else.
library;

import 'package:duet/duet.dart';
import 'package:duet/typed.dart';

/// A two-field struct, the Dart mirror of the fixture's Rust `Editor`.
final class Editor {
  /// Creates an editor.
  const Editor({required this.zoom, required this.mode});

  /// The zoom factor, an `f64` on the host.
  final double zoom;

  /// The mode name, a `String` on the host.
  final String mode;

  @override
  bool operator ==(Object other) =>
      other is Editor && other.zoom == zoom && other.mode == mode;

  @override
  int get hashCode => Object.hash(zoom, mode);

  @override
  String toString() => 'Editor(zoom: $zoom, mode: $mode)';
}

/// The codec for [Editor].
final class EditorCodec implements DuetCodec<Editor> {
  /// Creates the codec.
  const EditorCodec();

  @override
  String get name => 'Editor';

  @override
  DuetValue encode(Editor value) => DuetMap(<String, DuetValue>{
        'zoom': DuetFloat(value.zoom),
        'mode': DuetStr(value.mode),
      });

  @override
  Editor? decode(DuetValue value) {
    if (value is! DuetMap) return null;
    final DuetValue? zoom = value.entries['zoom'];
    final DuetValue? mode = value.entries['mode'];
    if (zoom is! DuetFloat || mode is! DuetStr) return null;
    return Editor(zoom: zoom.value, mode: mode.value);
  }
}

/// A codec that throws instead of answering, for the totality tests.
final class ThrowingCodec implements DuetCodec<String> {
  /// Creates the codec.
  const ThrowingCodec();

  @override
  String get name => 'Throwing';

  @override
  DuetValue encode(String value) => DuetStr(value);

  @override
  String? decode(DuetValue value) => throw StateError('codec bug');
}
