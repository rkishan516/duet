/**
 * The webview guest's UI.
 *
 * It replaces the page body outright, because it has to: `WebviewSurface` builds
 * its `wry` webview with `duet_webview::bootstrap::BOOTSTRAP_HTML` and exposes
 * no way to supply a page — see the "What the library could not do" section of
 * `examples/showcase/README.md`. So this guest arrives as a script, and the
 * first thing it does is build the document it wishes it had been loaded into.
 */

import { WEB_NOTE, type GuestView } from './guest.ts';

const STYLE = `
  :root { color-scheme: dark; }
  body {
    margin: 0; padding: 20px;
    font: 14px/1.5 -apple-system, system-ui, sans-serif;
    background: #16181d; color: #e6e8ee;
  }
  h1 { font-size: 18px; margin: 0 0 2px; }
  .host { color: #8f97a8; margin-bottom: 16px; }
  hr { border: 0; border-top: 1px solid #2b2f39; margin: 18px 0; }
  .row { display: flex; gap: 12px; padding: 3px 0; align-items: baseline; }
  .row .k { width: 180px; flex: none; color: #8f97a8; }
  .row .v { white-space: pre-wrap; word-break: break-word; }
  ul { margin: 4px 0 0 18px; padding: 0; }
  button {
    font: inherit; padding: 7px 14px; margin-right: 10px; border-radius: 7px;
    border: 1px solid #3a4050; background: #232734; color: inherit;
    cursor: pointer;
  }
  button.primary { background: #3d6fe0; border-color: #3d6fe0; }
  .note { color: #8f97a8; font-size: 12px; margin-top: 16px; }
`;

/** What the panel needs from the guest, beyond the view itself. */
export interface PanelActions {
  /** Appends a good line. */
  appendLine(): void;
  /** Appends a blank one, so the command raises. */
  appendBlank(): void;
  /**
   * Asks the host to do something only the host can do — see
   * `ShowcaseGuest.requestHost`. `null` when there is nobody listening (the
   * scripted tour), in which case the host-control section is not rendered.
   */
  requestHost: ((verb: string) => void) | null;
}

/**
 * Builds the page once and returns a function that redraws it.
 *
 * Built once and patched, rather than re-created per render, so the buttons keep
 * their identity and a click is never lost to a re-parse.
 */
export function mountPanel(actions: PanelActions): (view: GuestView) => void {
  const style = document.createElement('style');
  style.textContent = STYLE;
  document.head.append(style);
  document.title = 'Duet showcase — WebView guest';

  document.body.replaceChildren();
  const heading = el('h1', 'WebView guest');
  const host = el('div', '', 'host');
  const body = el('div', '');
  const good = button('append a line', 'primary', actions.appendLine);
  const blank = button('append a blank line (raises)', '', actions.appendBlank);
  const note = el(
    'div',
    'Both buttons call the same Rust #[command]. One returns, one raises a ' +
      'typed ComposeError.',
    'note',
  );
  const buttons = document.createElement('div');
  buttons.append(good, blank);
  document.body.append(heading, host, body, buttons, note);

  if (actions.requestHost) {
    const ask = actions.requestHost;
    const controlHeading = el('h1', 'Host controls');
    controlHeading.style.marginTop = '20px';
    const controls = document.createElement('div');
    controls.style.display = 'flex';
    controls.style.flexWrap = 'wrap';
    controls.style.gap = '8px';
    for (const [verb, label] of [
      ['suspend', 'suspend Flutter'],
      ['resume', 'resume Flutter'],
      ['teardown', 'tear Flutter down'],
      ['boot', 'boot Flutter again'],
      ['host_line', 'host: append a line'],
      ['sample', 'sample memory'],
    ] as const) {
      controls.append(button(label, '', () => ask(verb)));
    }
    const controlNote = el(
      'div',
      'These buttons write control.request into the store; the playground ' +
        'host watches it and obeys. Lifecycle belongs to the host — a guest ' +
        'can only ask.',
      'note',
    );
    document.body.append(hr(), controlHeading, controls, controlNote);
  }

  return (view: GuestView) => {
    host.textContent = `host: ${view.hostAct} — ${view.hostDetail}`;
    body.replaceChildren(
      row('document.title', view.title),
      row('document.lines', `${view.lines.length} line(s)`),
      list(view.lines),
      hr(),
      row('web.note (mine)', WEB_NOTE),
      row('flutter.note (the peer’s)', view.peerNote),
      hr(),
      row('returned', view.returned),
      row('raised', view.raised),
      ...(view.trouble ? [row('trouble', view.trouble)] : []),
    );
  };
}

function row(key: string, value: string): HTMLElement {
  const wrapper = el('div', '', 'row');
  wrapper.append(el('span', key, 'k'), el('span', value || '—', 'v'));
  return wrapper;
}

function list(lines: readonly string[]): HTMLElement {
  const ul = document.createElement('ul');
  for (const line of lines) {
    ul.append(el('li', line));
  }
  return ul;
}

function hr(): HTMLElement {
  return document.createElement('hr');
}

function el(tag: string, text: string, className = ''): HTMLElement {
  const node = document.createElement(tag);
  node.textContent = text;
  if (className) {
    node.className = className;
  }
  return node;
}

function button(
  label: string,
  className: string,
  onClick: () => void,
): HTMLButtonElement {
  const node = document.createElement('button');
  node.textContent = label;
  if (className) {
    node.className = className;
  }
  node.addEventListener('click', onClick);
  return node;
}
