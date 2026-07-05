import * as vscode from 'vscode';
import { DuetPanel } from './panel';
import { ServeClient } from './serveClient';
import { SessionsProvider } from './sessions';

export function activate(ctx: vscode.ExtensionContext): void {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;

  const provider = new SessionsProvider(root);
  ctx.subscriptions.push(vscode.window.registerTreeDataProvider('dtSessions', provider));

  const cfg = vscode.workspace.getConfiguration('dt');
  const client = new ServeClient(
    cfg.get('binaryPath', 'dt'),
    root ?? process.cwd(),
    cfg.get('writer', 'claude'),
  );
  ctx.subscriptions.push(client);

  ctx.subscriptions.push(
    vscode.commands.registerCommand('dt.newTask', () => {
      DuetPanel.createOrShow(ctx, client);
    }),
    vscode.commands.registerCommand('dt.refreshSessions', () => provider.refresh()),
    vscode.commands.registerCommand('dt.openSession', (dir: string) => {
      DuetPanel.createOrShow(ctx, client).showHistory(dir);
    }),
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
