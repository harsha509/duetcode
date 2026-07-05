import * as vscode from 'vscode';
import { spawn, ChildProcessWithoutNullStreams } from 'child_process';

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

  constructor(
    private readonly binPath: string,
    private readonly cwd: string,
    private readonly writerModel: string,
  ) {}

  start(): void {
    if (this.proc) {
      return;
    }
    const proc = spawn(this.binPath, ['serve', '--writer', this.writerModel], {
      cwd: this.cwd,
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
        message: `failed to start '${this.binPath}': ${err.message} — set dt.binaryPath in settings`,
      });
    });
    proc.on('exit', (code) => {
      this.proc = undefined;
      this._onExit.fire(code);
    });
  }

  send(obj: unknown): void {
    this.start();
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
