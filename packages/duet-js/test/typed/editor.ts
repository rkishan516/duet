/**
 * The hand-written schema type the typed tests share.
 *
 * Mirrors `packages/duet/test/typed/editor.dart`. Stands in for what increment
 * 4 will generate: a struct, a codec that is total and answers `null` rather
 * than throwing, and nothing else.
 *
 * @module
 */

import { duetFloat, duetMap, duetStr, type DuetValue } from '../../src/index.ts';
import type { DuetCodec } from '../../src/typed/index.ts';

/** A two-field struct, the TypeScript mirror of the fixture's Rust `Editor`. */
export interface Editor {
  /** The zoom factor, an `f64` on the host. */
  readonly zoom: number;
  /** The mode name, a `String` on the host. */
  readonly mode: string;
}

/** The codec for {@link Editor}. */
export const editorCodec: DuetCodec<Editor> = {
  name: 'Editor',
  encode: (value) =>
    duetMap(
      new Map<string, DuetValue>([
        ['zoom', duetFloat(value.zoom)],
        ['mode', duetStr(value.mode)],
      ]),
    ),
  decode: (value) => {
    if (value.kind !== 'map') return null;
    const zoom = value.entries.get('zoom');
    const mode = value.entries.get('mode');
    if (zoom?.kind !== 'float' || mode?.kind !== 'str') return null;
    return { zoom: zoom.value, mode: mode.value };
  },
};

/** A codec that throws instead of answering, for the totality tests. */
export const throwingCodec: DuetCodec<string> = {
  name: 'Throwing',
  encode: (value) => duetStr(value),
  decode: () => {
    throw new Error('codec bug');
  },
};
