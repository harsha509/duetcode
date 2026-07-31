import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import { execFile } from 'child_process';

/** Folders the user declined, remembered so the prompt never nags. */
const DECLINED_KEY = 'dt.initDeclined';

const INITIALIZE = 'Initialize';
const NOT_NOW = 'Not now';

/** Mirrors git::is_git_repo: a .git entry, directory or worktree file. */
function isGitRepo(dir: string): boolean {
  return fs.existsSync(path.join(dir, '.git'));
}

/** `dt serve` bails at startup without this file, so it gates the prompt. */
function isInitialized(dir: string): boolean {
  return fs.existsSync(path.join(dir, '.duet', 'config.toml'));
}

/** A workspace project that needs `dt init`. */
interface Candidate {
  name: string;
  dir: string;
}

function pendingProjects(declined: ReadonlySet<string>): Candidate[] {
  return (vscode.workspace.workspaceFolders ?? [])
    .map((folder) => ({ name: folder.name, dir: folder.uri.fsPath }))
    .filter(({ dir }) => isGitRepo(dir) && !isInitialized(dir) && !declined.has(dir));
}

function promptText(pending: Candidate[]): string {
  if (pending.length === 1) {
    return (
      `'${pending[0].name}' is not set up for DT Duet. Initialize it? ` +
      'This creates .duet/config.toml with the default prompts, and ignores .duet/sessions/.'
    );
  }
  const names = pending.map((p) => p.name).join(', ');
  return (
    `${pending.length} workspace projects are not set up for DT Duet (${names}). ` +
    'Initialize them? This creates .duet/config.toml with the default prompts, ' +
    'and ignores .duet/sessions/.'
  );
}

/** Runs `dt init` in one project. Rejects with the CLI's own error text. */
function runInit(binPath: string, cwd: string): Promise<void> {
  return new Promise((resolve, reject) => {
    execFile(binPath, ['init'], { cwd }, (err, _stdout, stderr) => {
      if (err) {
        reject(new Error(stderr.trim() || err.message));
        return;
      }
      resolve();
    });
  });
}

/** `${name} — ${reason}` for each project whose init failed. */
async function initAll(binPath: string, pending: Candidate[]): Promise<string[]> {
  const results = await Promise.all(
    pending.map(async ({ name, dir }) => {
      try {
        await runInit(binPath, dir);
        return undefined;
      } catch (e) {
        return `${name} — ${e instanceof Error ? e.message : String(e)}`;
      }
    }),
  );
  return results.filter((failure): failure is string => failure !== undefined);
}

async function rememberDeclines(
  ctx: vscode.ExtensionContext,
  declined: ReadonlySet<string>,
  pending: Candidate[],
): Promise<void> {
  const updated = new Set(declined);
  for (const { dir } of pending) {
    updated.add(dir);
  }
  await ctx.workspaceState.update(DECLINED_KEY, [...updated]);
}

/**
 * Offers `dt init` for every git project in the workspace that lacks a
 * .duet/config.toml, before `dt serve` is spawned against it — without one the
 * server exits 1 at startup. `dt init` is idempotent, so an accepted prompt
 * never disturbs a project that is already set up.
 */
export async function ensureInitialized(
  ctx: vscode.ExtensionContext,
  binPath: string,
): Promise<void> {
  const declined = new Set(ctx.workspaceState.get<string[]>(DECLINED_KEY, []));
  const pending = pendingProjects(declined);
  if (pending.length === 0) {
    return;
  }

  const choice = await vscode.window.showInformationMessage(
    promptText(pending),
    INITIALIZE,
    NOT_NOW,
  );
  if (choice === NOT_NOW) {
    await rememberDeclines(ctx, declined, pending);
    return;
  }
  if (choice !== INITIALIZE) {
    // Dismissed rather than declined: ask again next time.
    return;
  }

  const failures = await initAll(binPath, pending);
  if (failures.length > 0) {
    void vscode.window.showWarningMessage(`dt init failed — ${failures.join('; ')}`);
  }
}
