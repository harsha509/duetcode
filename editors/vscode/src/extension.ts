import * as vscode from 'vscode';
import { ensureInitialized, isGitRepo, isInitialized } from './init';
import { DuetPanel } from './panel';
import { ServeClient } from './serveClient';
import {
  allSessions,
  deleteSessions,
  isSessionDir,
  sessionLabel,
  SessionInfo,
  SessionNode,
  SessionsProvider,
} from './sessions';
import { secretEnv } from './settings';
import { SettingsPanel } from './settingsPanel';

/** Settings baked into the `dt serve` process, so changing one needs a respawn. */
const SPAWN_SETTINGS = ['dt.binaryPath', 'dt.writer', 'dt.claudeModel', 'dt.geminiModel'];

/** Absolute path of every folder in the workspace, in workspace order. */
function workspaceProjects(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath);
}

/**
 * Where `dt serve` is spawned. The server loads a config and checks for a git
 * repo at startup, so an uninitialized first folder would make it exit 1 before
 * it could be told about the workspace — pick a folder that satisfies it. The
 * first folder remains the fallback so an unsatisfiable workspace still fails
 * with the server's own message rather than a silently different one.
 */
function serveCwd(): string {
  const projects = workspaceProjects();
  return (
    projects.find((dir) => isGitRepo(dir) && isInitialized(dir)) ??
    projects.find(isGitRepo) ??
    projects[0] ??
    process.cwd()
  );
}

const STOP_IT = 'Stop It';

/**
 * Turns the new-session button away while a task is running, and offers the one
 * thing that would make it possible.
 *
 * The task cannot survive a new session — there is no second server to move it
 * to — and interrupting one to start another is how a round's work is lost
 * halfway through it.
 */
async function warnTaskStillRunning(): Promise<void> {
  const choice = await vscode.window.showWarningMessage(
    'A task is still running. Let it finish before starting a new session — a new ' +
      'session restarts the models, which would end this task mid-round.',
    STOP_IT,
  );
  if (choice === STOP_IT) {
    await vscode.commands.executeCommand('dt.stopTask');
  }
}

const DELETE = 'Delete';

/** A session's logs are gone for good once deleted, so both paths confirm first. */
async function confirmDelete(message: string): Promise<boolean> {
  const choice = await vscode.window.showWarningMessage(message, { modal: true }, DELETE);
  return choice === DELETE;
}

/**
 * Deletes and refreshes. The tree watcher refreshes too, but on its own delay —
 * a row left standing after the confirmation reads as the delete having failed.
 */
async function removeSessions(
  sessions: SessionInfo[],
  provider: SessionsProvider,
): Promise<void> {
  const failed = await deleteSessions(sessions);
  if (failed.length > 0) {
    void vscode.window.showErrorMessage(
      `DT Duet: could not delete ${failed.join(', ')} — check the file permissions on .duet/sessions.`,
    );
  }
  provider.refresh();
}

export function activate(ctx: vscode.ExtensionContext): void {
  const provider = new SessionsProvider();
  ctx.subscriptions.push(vscode.window.registerTreeDataProvider('dtSessions', provider));

  const client = new ServeClient(
    () => {
      const cfg = vscode.workspace.getConfiguration('dt');
      return {
        binPath: cfg.get('binaryPath', 'dt'),
        writer: cfg.get('writer', 'claude'),
        claudeModel: cfg.get('claudeModel', ''),
        geminiModel: cfg.get('geminiModel', ''),
      };
    },
    serveCwd,
    () => secretEnv(ctx),
    (binPath) => ensureInitialized(ctx, binPath),
    () => [{ cmd: 'workspace', projects: workspaceProjects() }],
  );
  ctx.subscriptions.push(client);

  // A folder added or removed changes which projects a task may read and write.
  // Sent to the running server rather than restarting it: a respawn would drop
  // both models' accumulated context for what is only a change of scope.
  ctx.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(() => {
      client.notify({ cmd: 'workspace', projects: workspaceProjects() });
    }),
  );

  // Spawn-time settings only take effect on a fresh `dt serve`; restart so the
  // next task picks up a changed writer, model, or binary path. Other dt
  // settings must not restart — that would cost the session its context.
  ctx.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (SPAWN_SETTINGS.some((setting) => e.affectsConfiguration(setting))) {
        client.restart();
      }
    }),
  );

  ctx.subscriptions.push(
    vscode.commands.registerCommand('dt.newTask', () => {
      // Refused rather than confirmed while a task runs: a new session kills
      // the server the task is running in, and waiting is nearly always what
      // was meant. Stopping is offered inside the warning for when it is not.
      if (client.isRunning) {
        void warnTaskStillRunning();
        return;
      }
      // A new session starts clean on both counts the user can see: the old
      // panel closes with its timeline, and `dt serve` is killed so neither
      // model resumes the conversation the previous session left it in.
      // Context belongs to the session window, so carrying it into a new one
      // would make two sessions that only look independent.
      DuetPanel.close();
      client.restart();
      DuetPanel.createOrShow(ctx, client);
    }),
    vscode.commands.registerCommand('dt.stopTask', () => {
      if (!client.isRunning) {
        void vscode.window.showInformationMessage('DT Duet: no task is running.');
        return;
      }
      client.stop();
    }),
    vscode.commands.registerCommand('dt.refreshSessions', () => provider.refresh()),
    vscode.commands.registerCommand('dt.openSession', (dir: string) => {
      DuetPanel.createOrShow(ctx, client).showHistory(dir);
    }),
    vscode.commands.registerCommand('dt.deleteSession', async (el?: SessionNode) => {
      // Reachable from another extension with any argument it likes, so the
      // target is checked to be a session of ours before anything is removed.
      if (el?.kind !== 'session' || !isSessionDir(el.dir)) {
        void vscode.window.showInformationMessage(
          'DT Duet: right-click a session in the Sessions view to delete it.',
        );
        return;
      }
      const ok = await confirmDelete(
        `Delete session "${sessionLabel(el)}"? Its logs are removed from disk.`,
      );
      if (ok) {
        await removeSessions([el], provider);
      }
    }),
    vscode.commands.registerCommand('dt.clearSessions', async (el?: SessionNode) => {
      // From a project row, that project's sessions; from the view title or the
      // palette, every folder's — the tree hides the project row when there is
      // only one folder, so the title button must not be limited to it.
      const targets = el?.kind === 'project' ? el.sessions : allSessions();
      if (targets.length === 0) {
        void vscode.window.showInformationMessage('DT Duet: no sessions to clear.');
        return;
      }
      const scope = el?.kind === 'project' ? ` in ${el.name}` : '';
      const ok = await confirmDelete(
        `Delete all ${targets.length} session${targets.length === 1 ? '' : 's'}${scope}? ` +
          'Their logs are removed from disk.',
      );
      if (ok) {
        await removeSessions(targets, provider);
      }
    }),
    vscode.commands.registerCommand('dt.settings', () => SettingsPanel.createOrShow(ctx, client)),
  );

  // Gates the stop button on there being something to stop, so the toolbar
  // carries it only while a task is actually running.
  ctx.subscriptions.push(
    client.onRunningChanged((running) => {
      void vscode.commands.executeCommand('setContext', 'dt.running', running);
    }),
  );

  ctx.subscriptions.push(watchSessions(provider));
}

/**
 * Refreshes the sessions tree when any workspace folder records one. Watching
 * only the first folder leaves a task run in a second project invisible until
 * the window is reloaded, so every folder gets a watcher — and the set is
 * rebuilt whenever folders are added or removed.
 */
function watchSessions(provider: SessionsProvider): vscode.Disposable {
  let watchers: vscode.FileSystemWatcher[] = [];

  const rebuild = (): void => {
    for (const watcher of watchers) {
      watcher.dispose();
    }
    watchers = (vscode.workspace.workspaceFolders ?? []).map((folder) => {
      const watcher = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(folder, '.duet/sessions/**'),
      );
      watcher.onDidCreate(() => provider.refresh());
      watcher.onDidChange(() => provider.refresh());
      watcher.onDidDelete(() => provider.refresh());
      return watcher;
    });
  };

  rebuild();
  const folderChange = vscode.workspace.onDidChangeWorkspaceFolders(() => {
    rebuild();
    provider.refresh();
  });

  return new vscode.Disposable(() => {
    folderChange.dispose();
    for (const watcher of watchers) {
      watcher.dispose();
    }
  });
}

export function deactivate(): void {}
