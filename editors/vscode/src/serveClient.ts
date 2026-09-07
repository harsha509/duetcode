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

/** What the server is busy with, for the sessions tree. */
export interface Activity {
  /** The task text, or 'review' for a standalone review. */
  label: string;
  /** Absolute path of the project the work runs in. */
  project: string;
  /** When the run started, in epoch ms. */
  since: number;
}

/** The fields of a server event the client itself reads. */
interface ServeEvent {
  event?: string;
  task?: string;
  path?: string;
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

  /** Whether a task or review is holding the server. See `trackRunning`. */
  private running = false;
  private readonly _onRunningChanged = new vscode.EventEmitter<boolean>();
  readonly onRunningChanged = this._onRunningChanged.event;

  /** What is holding it, while something is. See `trackRunning`. */
  private activity?: Activity;
  private readonly _onActivityChanged = new vscode.EventEmitter<Activity | undefined>();
  readonly onActivityChanged = this._onActivityChanged.event;

  private starting?: Promise<void>;

  get isRunning(): boolean {
    return this.running;
  }

  get currentActivity(): Activity | undefined {
    return this.activity;
  }

  private setActivity(activity: Activity | undefined): void {
    this.activity = activity;
    this._onActivityChanged.fire(activity);
    this.setRunning(activity !== undefined);
  }

  private setRunning(running: boolean): void {
    if (this.running === running) {
      return;
    }
    this.running = running;
    this._onRunningChanged.fire(running);
  }

  /**
   * Follows a run's life through the event stream, which is the only place it
   * is visible: the serve loop is busy from `project_started` — sent for a task
   * and for each project a review covers — until it reports an outcome, and an
   * `error` ends the run that raised it. A task's `task_started` follows and
   * names it; a review has no such event, so it is labelled as one here.
   */
  private trackRunning(event: ServeEvent): void {
    if (event.event === 'project_started') {
      this.setActivity({ label: 'review', project: event.path ?? '', since: Date.now() });
    } else if (event.event === 'task_started') {
      const running = this.activity;
      this.setActivity({
        label: event.task ?? '',
        project: running?.project ?? '',
        since: running?.since ?? Date.now(),
      });
    } else if (event.event === 'task_done' || event.event === 'error') {
      this.setActivity(undefined);
    }
  }

  /**
   * Asks the server to stop the running task or review. It cancels the turn,
   * kills the model it was waiting on, and reports `task_done` — the models'
   * context survives, unlike a restart.
   */
  stop(): void {
    this.notify({ cmd: 'stop' });
  }

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
    // Whatever was running died with the process. Cleared before the early
    // return so a client that never spawned cannot be left claiming otherwise.
    this.setActivity(undefined);
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
        let event: ServeEvent;
        try {
          event = JSON.parse(line);
        } catch {
          continue; // non-JSON noise on stdout
        }
        this.trackRunning(event);
        this._onEvent.fire(event);
      }
    });

    // stderr carries human-oriented decorations only.
    proc.stderr.setEncoding('utf8');
    proc.stderr.on('data', () => {});

    proc.on('error', (err: NodeJS.ErrnoException) => {
      this.proc = undefined;
      this.setActivity(undefined);
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
      // A crash mid-task ends the task, and leaving `running` set would lock
      // out the new-session button with nothing left to be running.
      this.setActivity(undefined);
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
