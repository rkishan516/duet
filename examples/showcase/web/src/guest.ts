/**
 * The webview guest's side of the Duet conversation.
 *
 * Everything below goes through the client generated in `showcase.duet.ts`.
 * There is not one hand-written path string in this file, and the shape mirrors
 * the Flutter guest's `lib/src/guest.dart` almost line for line — which is the
 * claim the showcase is making: one Rust definition, two renderers, the same
 * conversation.
 */

import { DuetFailure, type DuetClient } from 'duet-protocol';
import {
  DuetRouter,
  type DuetReading,
} from 'duet-protocol/typed';

import {
  ShowcaseClient,
  ShowcaseCommands,
  type ComposeError,
  type HostNote,
} from './showcase.duet.ts';

/** The line this guest contributes to the shared document. */
export const WEB_LINE = 'WebView: the same store, a different renderer.';

/** A line that cannot be composed, so `append_line` raises rather than returns. */
export const BLANK_LINE = '   ';

/** The greeting this guest publishes at `web.note` for the Flutter guest. */
export const WEB_NOTE = 'hello from the WebView';

/** Everything this guest has read, written, or been told. */
export interface GuestView {
  title: string;
  lines: readonly string[];
  peerNote: string;
  hostAct: string;
  hostDetail: string;
  returned: string;
  raised: string;
  trouble: string;
}

/** The starting view, before any push has arrived. */
export function emptyView(): GuestView {
  return {
    title: '',
    lines: [],
    peerNote: '',
    hostAct: '',
    hostDetail: '',
    returned: '',
    raised: '',
    trouble: '',
  };
}

/** Drives the webview guest and reports every change to one listener. */
export class ShowcaseGuest {
  readonly state: ShowcaseClient;
  readonly commands: ShowcaseCommands;

  #view: GuestView = emptyView();
  readonly #onChange: (view: GuestView) => void;

  /**
   * Binds a guest to an already-started client.
   *
   * `attach()` takes ownership of the client's push slot and calls `start()`
   * itself; nothing else may assign `client.onPush` while it holds it.
   */
  constructor(client: DuetClient, onChange: (view: GuestView) => void) {
    const router = new DuetRouter(client);
    router.attach();
    this.state = new ShowcaseClient(router);
    this.commands = new ShowcaseCommands(client);
    this.#onChange = onChange;
  }

  /** The current view. */
  get view(): GuestView {
    return this.#view;
  }

  /**
   * Subscribes to everything this guest displays, then runs its opening moves.
   *
   * Watchers are armed before the first command, so the push produced by this
   * guest's own `append_line` is one it is already listening for.
   */
  async start(): Promise<void> {
    await this.#watchEverything();
    await this.#publish(() => this.state.web.note.set(WEB_NOTE), 'web.note');
    await this.appendLine(WEB_LINE);
    await this.appendLine(BLANK_LINE);
    // Written last: the host treats `web.status === "ready"` as "this guest has
    // finished its opening moves", so setting it earlier would let the host read
    // half a story.
    await this.#publish(
      () => this.state.web.status.set('ready'),
      'web.status',
    );
  }

  /** Invokes `append_line` and records whichever arm came back. */
  async appendLine(text: string): Promise<string> {
    let rendered = '';
    try {
      const outcome = await this.commands.appendLine({ text });
      switch (outcome.kind) {
        case 'ok':
          rendered = `append_line('${text}') returned ${outcome.value}`;
          this.#update({ returned: rendered });
          await this.#publish(
            () => this.state.web.returned.set(rendered),
            'web.returned',
          );
          break;
        case 'err': {
          const error: ComposeError = outcome.error;
          rendered = `append_line('${text}') raised ${error.code}: ${error.detail}`;
          this.#update({ raised: rendered });
          await this.#publish(
            () => this.state.web.raised.set(rendered),
            'web.raised',
          );
          break;
        }
        case 'undecodable':
          rendered =
            `append_line('${text}') answered something the schema does not ` +
            `describe (raised: ${outcome.raised})`;
          this.#update({ trouble: rendered });
          break;
      }
    } catch (cause) {
      // A refusal, not a raise: the host declined to run the command at all.
      // `DuetErr` means it ran; this means the two sides disagree about what
      // exists, which no amount of retrying fixes.
      rendered =
        cause instanceof DuetFailure
          ? `append_line('${text}') was refused: ${cause.message}`
          : `append_line('${text}') failed: ${String(cause)}`;
      this.#update({ trouble: rendered });
    }
    return rendered;
  }

  /**
   * Arms every watcher, then feeds each one the snapshot it was created with.
   *
   * The second half is load-bearing and easy to miss. A callback fires only for
   * changes *after* the subscription; the value the path already held arrives as
   * `DuetWatch.current` the instant `watch` resolves. A guest that attaches
   * after its peer has already written would otherwise never learn what the peer
   * wrote.
   */
  async #watchEverything(): Promise<void> {
    const onTitle = (reading: DuetReading<string>) => {
      this.#update({ title: text(reading) });
    };
    onTitle((await this.state.document.title.watch(onTitle)).current);

    const onLines = (reading: DuetReading<string[]>) => {
      const lines = reading.kind === 'present' ? reading.value : [];
      this.#update({ lines });
      // Mirrored into the store so the host — which has no screen to look at —
      // can see that this guest's watcher actually fired.
      void this.#publish(
        () => this.state.web.sawLines.set(BigInt(lines.length)),
        'web.saw_lines',
      );
    };
    onLines((await this.state.document.lines.watch(onLines)).current);

    const onPeerNote = (reading: DuetReading<string>) => {
      const peerNote = text(reading);
      this.#update({ peerNote });
      void this.#publish(
        () => this.state.web.sawPeerNote.set(peerNote),
        'web.saw_peer_note',
      );
    };
    onPeerNote((await this.state.flutter.note.watch(onPeerNote)).current);

    const onHostNote = (reading: DuetReading<HostNote>) => {
      if (reading.kind === 'present') {
        this.#update({
          hostAct: reading.value.act,
          hostDetail: reading.value.detail,
        });
      }
    };
    onHostNote((await this.state.host.self.watch(onHostNote)).current);
  }

  async #publish(write: () => Promise<void>, what: string): Promise<void> {
    try {
      await write();
    } catch (cause) {
      // Never rethrow: half of these run from a push handler, and an unhandled
      // rejection there would destroy the evidence the host is about to read.
      this.#update({ trouble: `writing ${what} failed: ${String(cause)}` });
    }
  }

  #update(patch: Partial<GuestView>): void {
    this.#view = { ...this.#view, ...patch };
    this.#onChange(this.#view);
  }
}

/**
 * Renders a four-way reading as one line.
 *
 * A read is never an exception: `present`, `none` (an explicit null), `absent`
 * (no node at all), and `mismatch` (another guest wrote a type this codec
 * refuses) are four states a UI has to be able to draw.
 */
function text(reading: DuetReading<string>): string {
  switch (reading.kind) {
    case 'present':
      return reading.value;
    case 'none':
      return '(null)';
    case 'absent':
      return '(absent)';
    case 'mismatch':
      return `(mismatch: ${reading.reason})`;
  }
}
