/**
 * The webview guest of the Duet showcase.
 *
 * Bundled to `build/guest.js` and evaluated into the surface's page by the Rust
 * host (`WebviewSurface::eval`). That is not how a real app would ship a page —
 * see the README — but the code below is exactly what a real app would write:
 * connect a transport, attach a router, use the generated client.
 *
 * ```console
 * $ (cd examples/showcase/web && npm install && npm run build)
 * ```
 */

import { connectWryDuet } from 'duet-protocol/wry';

import { BLANK_LINE, ShowcaseGuest, WEB_LINE, emptyView } from './guest.ts';
import { mountPanel } from './panel.ts';

/** What the host's readback probe reads to find out how this guest is doing. */
interface ShowcaseMarker {
  /** True once the panel is on screen and the guest is wired up. */
  mounted: boolean;
  /** The last thing that went wrong, or `null`. */
  trouble: string | null;
}

declare global {
  interface Window {
    /** Set by this bundle; read by the Rust host over `eval_with_callback`. */
    __duetShowcase?: ShowcaseMarker;
  }
}

function boot(): void {
  const marker: ShowcaseMarker = { mounted: false, trouble: null };
  window.__duetShowcase = marker;

  try {
    // Replaces the bootstrap page's `window.__duet` with a transport that
    // correlates replies properly. The host's reply path is
    // `window.__duet && window.__duet.onResponse(...)` — a fresh lookup on
    // `window` every time — so swapping the object is all it takes, and it must
    // happen before the host sends anything this guest asked for.
    const client = connectWryDuet();

    const render = mountPanel({
      appendLine: () => void guest.appendLine(WEB_LINE),
      appendBlank: () => void guest.appendLine(BLANK_LINE),
    });
    const guest = new ShowcaseGuest(client, render);
    render(emptyView());
    marker.mounted = true;

    void guest.start().catch((cause: unknown) => {
      marker.trouble = String(cause);
    });
  } catch (cause) {
    // The host has no console here and no screen to look at, so a boot failure
    // has to be left somewhere it can read.
    marker.trouble = String(cause);
  }
}

// Evaluated, not loaded: the host may `eval` this bundle more than once if it
// cannot tell whether the first attempt landed, and booting twice would install
// a second router over the same client and throw.
if (!window.__duetShowcase) {
  boot();
}
