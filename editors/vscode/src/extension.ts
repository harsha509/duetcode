import * as vscode from 'vscode';
import { ensureInitialized } from './init';
import { DuetPanel } from './panel';
import { ServeClient } from './serveClient';
import { SessionsProvider } from './sessions';
import { secretEnv } from './settings';
import { SettingsPanel } from './settingsPanel';

export function activate(ctx: vscode.ExtensionContext): void {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

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
    root ?? process.cwd(),
    () => secretEnv(ctx),
    (binPath) => ensureInitialized(ctx, binPath),
  );
  ctx.subscriptions.push(client);

  // Settings only take effect on a fresh `dt serve` spawn; restart so the
  // next task picks up a changed writer, model, or binary path.
  ctx.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('dt')) {
        client.restart();
      }
    }),
  );

  ctx.subscriptions.push(
    vscode.commands.registerCommand('dt.newTask', () => {
      DuetPanel.createOrShow(ctx, client);
    }),
    vscode.commands.registerCommand('dt.refreshSessions', () => provider.refresh()),
    vscode.commands.registerCommand('dt.openSession', (dir: string) => {
      DuetPanel.createOrShow(ctx, client).showHistory(dir);
    }),
    vscode.commands.registerCommand('dt.settings', () => SettingsPanel.createOrShow(ctx, client)),
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
