/**
 * A `DuetTransport` over the real Rust host, running as a child process.
 *
 * Mirrors `packages/duet/test/support/live_host.dart`.
 *
 * `crates/duet-host-stdio` wraps `duet_protocol::handle_text` in a process that
 * speaks newline-delimited JSON on stdin and stdout. This is the JavaScript
 * side of that pipe, and it is what lets `live-host.test.ts` drive the
 * **generated** client against the host rather than against a fake transcribed
 * from `crates/duet-core/src/value.rs`.
 *
 * # Correlation lives here, as the transport contract requires
 *
 * `DuetTransport.send` must return a promise bound to *this* message's reply;
 * `DuetClient` keeps no pending map. A single pipe carries every reply and every
 * push interleaved, so this class reads the outgoing request's `id` and keys a
 * resolver by it.
 *
 * # Nothing here may hang
 *
 * `node --test` has **no default per-test timeout**, so an unresolved promise
 * wedges the whole run rather than failing one case — this project has been
 * bitten by exactly that. Three guards, and every `test()` in `live-host.test.ts`
 * also passes an explicit `timeout`:
 *
 * - every `send` is bounded by {@link REPLY_TIMEOUT_MS};
 * - the process exiting rejects every pending call, naming its exit code and
 *   whatever it wrote to stderr;
 * - a reply whose id matches nothing is recorded in `unmatched` rather than
 *   dropped, so a test can assert the stream held no surprises.
 *
 * @module
 */

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createInterface, type Interface } from 'node:readline';

import type { DuetTransport } from '../../src/index.ts';

/** How long one request may wait for its reply. */
export const REPLY_TIMEOUT_MS = 10_000;

/**
 * The environment variable that names the host binary.
 *
 * When it is set, a binary that cannot be found is a **failure**, never a skip:
 * CI sets this, and a typo in it would otherwise turn the entire live-host
 * suite into silence that still exits zero.
 */
export const HOST_PATH_VARIABLE = 'DUET_HOST_STDIO';

/** How to build the host, named in every diagnostic that cannot find it. */
export const BUILD_COMMAND = 'cargo build -p duet-host-stdio';

/**
 * Where the binary lands under a plain `cargo test` or `cargo build`, relative
 * to this package's root — which is the working directory `npm test` runs in.
 */
const DEFAULT_HOST_PATHS = [
  '../../target/debug/duet-host-stdio',
  '../../target/release/duet-host-stdio',
];

/**
 * The host binary, or `null` if there is none to be found.
 *
 * @throws if {@link HOST_PATH_VARIABLE} names a file that is not there.
 */
export function locateDuetHost(): string | null {
  const named = process.env[HOST_PATH_VARIABLE];
  if (named !== undefined && named !== '') {
    if (!existsSync(named)) {
      throw new Error(
        `${HOST_PATH_VARIABLE} names "${named}", which does not exist. An ` +
          'explicit override must not silently skip the conformance run. ' +
          `Build the host with:\n    ${BUILD_COMMAND}`,
      );
    }
    return named;
  }
  for (const candidate of DEFAULT_HOST_PATHS) {
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

/** Why the live-host tests are being skipped, or `false` if they can run. */
export function liveHostSkip(): string | false {
  if (locateDuetHost() !== null) return false;
  return (
    'the duet-host-stdio binary was not found; build it with ' +
    `\`${BUILD_COMMAND}\`, or set ${HOST_PATH_VARIABLE} to its path`
  );
}

/** A transport over a running `duet-host-stdio` process. */
export class StdioHost implements DuetTransport {
  /**
   * Starts a host seeded from the named schema fixture.
   *
   * @throws if no binary can be found.
   */
  static start(schema: string): StdioHost {
    const binary = locateDuetHost();
    if (binary === null) {
      throw new Error(`no duet-host-stdio binary; build it with \`${BUILD_COMMAND}\``);
    }
    return new StdioHost(binary, schema);
  }

  /**
   * Every line the host sent that answered no outstanding request and was not a
   * push.
   *
   * Empty in a correct run. Recorded rather than dropped because a reply that
   * matches nothing is the shape of a correlation bug, and silence about it is
   * how such a bug survives.
   */
  readonly unmatched: string[] = [];

  readonly #process: ChildProcessWithoutNullStreams;
  readonly #lines: Interface;
  readonly #schema: string;
  readonly #pending = new Map<string, (line: string) => void>();
  readonly #failures = new Map<string, (reason: Error) => void>();
  #errors = '';
  #onPush: ((message: string) => void) | null = null;
  #stopped: string | null = null;

  private constructor(binary: string, schema: string) {
    this.#schema = schema;
    this.#process = spawn(binary, [schema], { stdio: ['pipe', 'pipe', 'pipe'] });
    this.#lines = createInterface({ input: this.#process.stdout });
    this.#lines.on('line', (line) => {
      this.#receive(line);
    });
    this.#process.stderr.setEncoding('utf8');
    this.#process.stderr.on('data', (chunk: string) => {
      this.#errors += chunk;
    });
    this.#process.on('exit', (code, signal) => {
      this.#stop(`the host exited with code ${String(code)} (signal ${String(signal)})`);
    });
    this.#process.on('error', (error: Error) => {
      this.#stop(`the host could not be started: ${error.message}`);
    });
  }

  /** The schema fixture this host was seeded from. */
  get schema(): string {
    return this.#schema;
  }

  set onPush(handler: ((message: string) => void) | null) {
    this.#onPush = handler;
  }

  send(request: string): Promise<string | null> {
    if (this.#stopped !== null) {
      return Promise.reject(new Error(`${this.#stopped}${this.#diagnostic()}`));
    }
    const id = idOf(request);
    if (this.#pending.has(id)) {
      return Promise.reject(new Error(`request id ${id} is already outstanding`));
    }
    return new Promise<string | null>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        this.#failures.delete(id);
        reject(
          new Error(
            `the host for "${this.#schema}" did not answer request ${id} within ` +
              `${String(REPLY_TIMEOUT_MS)}ms${this.#diagnostic()}`,
          ),
        );
      }, REPLY_TIMEOUT_MS);
      this.#pending.set(id, (line) => {
        clearTimeout(timer);
        this.#failures.delete(id);
        resolve(line);
      });
      this.#failures.set(id, (reason) => {
        clearTimeout(timer);
        this.#pending.delete(id);
        reject(reason);
      });
      this.#process.stdin.write(`${request}\n`);
    });
  }

  /** Stops the host and releases its streams. Idempotent. */
  async close(): Promise<void> {
    if (this.#stopped === null) this.#stop('the host was closed by the test');
    this.#process.stdin.end();
    await new Promise<void>((resolve) => {
      if (this.#process.exitCode !== null || this.#process.signalCode !== null) {
        resolve();
        return;
      }
      // The host exits on end of input; kill only if it does not, so a hung
      // host fails a test rather than hanging the suite.
      const timer = setTimeout(() => {
        this.#process.kill('SIGKILL');
      }, 5_000);
      this.#process.once('exit', () => {
        clearTimeout(timer);
        resolve();
      });
    });
    this.#lines.close();
  }

  /** Routes one line from the host. */
  #receive(line: string): void {
    let decoded: unknown;
    try {
      decoded = JSON.parse(line);
    } catch {
      this.unmatched.push(line);
      return;
    }
    if (typeof decoded !== 'object' || decoded === null) {
      this.unmatched.push(line);
      return;
    }
    const message = decoded as { kind?: unknown; id?: unknown };
    // A push is told from a reply by its envelope kind, which is the same rule
    // every other Duet transport uses. `duet_protocol::Push` has exactly one
    // arm, and a guest that guessed by the absence of an `id` would break the
    // day it gained a second.
    if (message.kind === 'notification') {
      this.#onPush?.(line);
      return;
    }
    const id = typeof message.id === 'string' ? message.id : null;
    const waiting = id === null ? undefined : this.#pending.get(id);
    if (waiting === undefined || id === null) {
      this.unmatched.push(line);
      return;
    }
    this.#pending.delete(id);
    waiting(line);
  }

  /** Rejects every outstanding call, so nothing waits forever. */
  #stop(because: string): void {
    this.#stopped = because;
    const failing = [...this.#failures.values()];
    this.#failures.clear();
    this.#pending.clear();
    for (const fail of failing) {
      fail(new Error(`${because}${this.#diagnostic()}`));
    }
  }

  /** Whatever the host said on stderr. */
  #diagnostic(): string {
    const errors = this.#errors.trim();
    return errors === '' ? '' : `\nhost stderr: ${errors}`;
  }
}

/** The `id` of an outgoing request. */
function idOf(request: string): string {
  const decoded: unknown = JSON.parse(request);
  if (typeof decoded !== 'object' || decoded === null) {
    throw new Error(`cannot correlate a request that is not an object: ${request}`);
  }
  const id = (decoded as { id?: unknown }).id;
  if (typeof id !== 'string') {
    throw new Error(`cannot correlate a request with no string id: ${request}`);
  }
  return id;
}
