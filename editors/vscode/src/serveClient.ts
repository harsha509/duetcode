import * as vscode from 'vscode';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';

/** How users install the `dt` binary the extension drives. */
const INSTALL_COMMAND = 'cargo install --git https://github.com/harsha509/duetcode.git';

const COPY_COMMAND = 'Copy Install Command';
const OPEN_SETTINGS = 'Open DT Settings';

/** Node surfaces a missing executable as ENOENT on the spawn error event. */
function isMissingBinary(err: NodeJS.ErrnoException): boolean {
  return err.code === 'ENOENT';
}

function startupErrorMessage(err: NodeJS.ErrnoException, binPath: string): string {
  if (isMissingBinary(err)) {
    return (
      `'${binPath}' not found — DT Duet needs the dt binary. Install it with ` +
      `\`${INSTALL_COMMAND}\`, then set dt.binaryPath (e.g. ~/.cargo/bin/dt) ` +
      `if it is still off VS Code's PATH.`
    );
  }
  return `failed to start '${binPath}': ${err.message} — set dt.binaryPath in settings`;
}

/**
 * The timeline may be scrolled away when the spawn fails, so the install
 * instruction also goes out as a notification the user cannot miss.
 */
async function offerInstallInstructions(binPath: string): Promise<void> {
  const choice = await vscode.window.showErrorMessage(
    `DT Duet could not find the dt binary ('${binPath}'). Install it with: ${INSTALL_COMMAND}`,
    COPY_COMMAND,
    OPEN_SETTINGS,
  );
  if (choice === COPY_COMMAND) {
    await vscode.env.clipboard.writeText(INSTALL_COMMAND);
  } else if (choice === OPEN_SETTINGS) {
    await vscode.commands.executeCommand('dt.settings');
  }
}

/** Spawn options read fresh from settings each time the server starts. */
export interface ServeOptions {
  binPath: string;
  writer: string;
  claudeModel: string;
  geminiModel: string;
}

/**
 * Thin client for `dt serve`: spawns the binary once, sends JSON-line
 * commands on stdin, and emits parsed JSON events from stdout.
 */
export class ServeClient implements vscode.Disposable {
  private proc?: ChildProcessWithoutNullStreams;
  private buffer = '';
  private readonly _onEvent = new vscode.EventEmitter<any>();
  readonly onEvent = this._onEvent.event;
  private readonly _onExit = new vscode.EventEmitter<number | null>();
  readonly onExit = this._onExit.event;

  private starting?: Promise<void>;

  constructor(
    /** Settings snapshot taken at spawn time, so restart() picks up changes. */
    private readonly optionsProvider: () => ServeOptions,
    /** Read per spawn: the folder set can change between one server and the next. */
    private readonly cwdProvider: () => string,
    /** Extra env (API keys from SecretStorage) injected at spawn time. */
    private readonly envProvider: () => Promise<Record<string, string>> = async () => ({}),
    /** Runs before each spawn, to set up projects the server would bail on. */
    private readonly prepare: (binPath: string) => Promise<void> = async () => {},
    /**
     * Commands written ahead of anything queued, re-sent on every spawn so a
     * restarted server is told about the workspace before it runs a task.
     */
    private readonly handshakeProvider: () => unknown[] = () => [],
  ) {}

  /** Kill the server; the next send() respawns it with fresh secrets. */
  restart(): void {
    const proc = this.proc;
    this.proc = undefined;
    // A partial line from the dead server would otherwise prefix — and
    // invalidate — the first event the next one writes.
    this.buffer = '';
    if (!proc) {
      return;
    }
    // Detached before the kill, not after: every one of these fires a tick or
    // more later, by which time a new task has a fresh panel listening. The
    // exit would read there as the new server having died, and whatever stdout
    // still flushes would replay the old session's events into it.
    proc.stdout.removeAllListeners('data');
    proc.removeAllListeners('exit');
    try {
      proc.kill();
    } catch {
      // already gone
    }
  }

  private ensureStarted(): Promise<void> {
    if (this.proc) {
      return Promise.resolve();
    }
    this.starting ??= this.start().finally(() => {
      this.starting = undefined;
    });
    return this.starting;
  }

  private async start(): Promise<void> {
    if (this.proc) {
      return;
    }
    const opts = this.optionsProvider();
    await this.prepare(opts.binPath);
    const extraEnv = await this.envProvider();
    const args = ['serve', '--writer', opts.writer];
    if (opts.claudeModel) {
      args.push('--claude-model', opts.claudeModel);
    }
    if (opts.geminiModel) {
      args.push('--gemini-model', opts.geminiModel);
    }
    const proc = spawn(opts.binPath, args, {
      cwd: this.cwdProvider(),
      env: { ...process.env, ...extraEnv },
    });
    this.proc = proc;

    proc.stdout.setEncoding('utf8');
    proc.stdout.on('data', (chunk: string) => {
      this.buffer += chunk;
      let idx: number;
      while ((idx = this.buffer.indexOf('\n')) >= 0) {
        const line = this.buffer.slice(0, idx).trim();
        this.buffer = this.buffer.slice(idx + 1);
        if (!line) {
          continue;
        }
        try {
          this._onEvent.fire(JSON.parse(line));
        } catch {
          // non-JSON noise on stdout; ignore
        }
      }
    });

    // stderr carries human-oriented decorations only.
    proc.stderr.setEncoding('utf8');
    proc.stderr.on('data', () => {});

    proc.on('error', (err: NodeJS.ErrnoException) => {
      this.proc = undefined;
      this._onEvent.fire({
        event: 'error',
        message: startupErrorMessage(err, opts.binPath),
      });
      if (isMissingBinary(err)) {
        void offerInstallInstructions(opts.binPath);
      }
    });
    proc.on('exit', (code) => {
      this.proc = undefined;
      this._onExit.fire(code);
    });

    // Written here rather than via send(), which would await ensureStarted()
    // and deadlock inside start(). Going out first also guarantees the server
    // knows the workspace before the command that made us spawn arrives.
    for (const cmd of this.handshakeProvider()) {
      this.write(cmd);
    }
  }

  send(obj: unknown): void {
    void this.ensureStarted().then(() => this.write(obj));
  }

  /**
   * Update a running server, starting nothing. Adding a workspace folder must
   * not launch `dt serve` on its own — if none is running, the handshake will
   * carry the change into the next spawn anyway.
   */
  notify(obj: unknown): void {
    if (this.proc) {
      this.write(obj);
    }
  }

  private write(obj: unknown): void {
    this.proc?.stdin.write(JSON.stringify(obj) + '\n');
  }

  dispose(): void {
    try {
      this.proc?.stdin.write(JSON.stringify({ cmd: 'quit' }) + '\n');
      this.proc?.kill();
    } catch {
      // already gone
    }
  }
}
