import * as vscode from 'vscode';
import { ensureInitialized } from './init';
import { DuetPanel } from './panel';
import { ServeClient } from './serveClient';
import { SessionsProvider } from './sessions';
import { secretEnv } from './settings';
import { SettingsPanel } from './settingsPanel';

export function activate(ctx: vscode.ExtensionContext): void {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

  const provider = new SessionsProvider(root);
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

  if (root) {
    const watcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(root, '.duet/sessions/**'),
    );
    watcher.onDidCreate(() => provider.refresh());
    watcher.onDidChange(() => provider.refresh());
    watcher.onDidDelete(() => provider.refresh());
    ctx.subscriptions.push(watcher);
  }
}

export function deactivate(): void {}
