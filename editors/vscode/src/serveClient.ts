import * as vscode from 'vscode';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';

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
    private readonly cwd: string,
    /** Extra env (API keys from SecretStorage) injected at spawn time. */
    private readonly envProvider: () => Promise<Record<string, string>> = async () => ({}),
  ) {}

  /** Kill the server; the next send() respawns it with fresh secrets. */
  restart(): void {
    try {
      this.proc?.kill();
    } catch {
      // already gone
    }
    this.proc = undefined;
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
    const extraEnv = await this.envProvider();
    const opts = this.optionsProvider();
    const args = ['serve', '--writer', opts.writer];
    if (opts.claudeModel) {
      args.push('--claude-model', opts.claudeModel);
    }
    if (opts.geminiModel) {
      args.push('--gemini-model', opts.geminiModel);
    }
    const proc = spawn(opts.binPath, args, {
      cwd: this.cwd,
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

    proc.on('error', (err) => {
      this.proc = undefined;
      this._onEvent.fire({
        event: 'error',
        message: `failed to start '${opts.binPath}': ${err.message} — set dt.binaryPath in settings`,
      });
    });
    proc.on('exit', (code) => {
      this.proc = undefined;
      this._onExit.fire(code);
    });
  }

  send(obj: unknown): void {
    void this.ensureStarted().then(() => {
      this.proc?.stdin.write(JSON.stringify(obj) + '\n');
    });
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
