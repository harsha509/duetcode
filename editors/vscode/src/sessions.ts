import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';

export interface SessionInfo {
  kind: 'session';
  dir: string;
  name: string;
  /** Workspace folder this session was recorded in. */
  project: string;
  state?: SessionState;
}

/** Groups one folder's sessions. Shown only in a multi-root workspace. */
export interface ProjectNode {
  kind: 'project';
  name: string;
  sessions: SessionInfo[];
}

export type SessionNode = ProjectNode | SessionInfo;

interface SessionState {
  task?: string;
  success?: boolean;
  final_verdict?: string;
  total_rounds?: number;
}

/**
 * Sidebar tree of past duet sessions, read straight from .duet/sessions.
 *
 * Every workspace folder is read, not just the first: a task runs in the
 * project the composer picked and its session is written under *that* repo, so
 * reading one folder hides — and appears to lose — every session belonging to
 * the others. Folders are re-read on each expansion rather than captured at
 * activation, so adding or removing one takes effect immediately.
 */
export class SessionsProvider implements vscode.TreeDataProvider<SessionNode> {
  private readonly _onDidChange = new vscode.EventEmitter<SessionNode | undefined>();
  readonly onDidChangeTreeData = this._onDidChange.event;

  refresh(): void {
    this._onDidChange.fire(undefined);
  }

  getTreeItem(el: SessionNode): vscode.TreeItem {
    return el.kind === 'project' ? projectTreeItem(el) : sessionTreeItem(el);
  }

  getChildren(el?: SessionNode): SessionNode[] {
    if (el) {
      return el.kind === 'project' ? el.sessions : [];
    }

    const folders = vscode.workspace.workspaceFolders ?? [];
    const projects = folders
      .map((folder) => readProject(folder))
      .filter((project) => project.sessions.length > 0);

    // One folder needs no grouping node — it would hold every session behind
    // an extra click and name a repo the user already knows.
    return folders.length > 1 ? projects : projects[0]?.sessions ?? [];
  }
}

function projectTreeItem(el: ProjectNode): vscode.TreeItem {
  const item = new vscode.TreeItem(el.name, vscode.TreeItemCollapsibleState.Expanded);
  item.description = `${el.sessions.length} session${el.sessions.length === 1 ? '' : 's'}`;
  item.iconPath = new vscode.ThemeIcon('repo');
  item.contextValue = 'dtProject';
  return item;
}

function sessionTreeItem(el: SessionInfo): vscode.TreeItem {
  const item = new vscode.TreeItem(labelFor(el), vscode.TreeItemCollapsibleState.None);
  item.description = descriptionFor(el);
  item.tooltip = `${el.project}\n${el.state?.task ?? el.name}`;
  item.command = {
    command: 'dt.openSession',
    title: 'Open Session',
    arguments: [el.dir],
  };
  item.iconPath = new vscode.ThemeIcon(
    el.state ? (el.state.success ? 'pass' : 'circle-slash') : 'circle-outline',
  );
  return item;
}

function readProject(folder: vscode.WorkspaceFolder): ProjectNode {
  return {
    kind: 'project',
    name: folder.name,
    sessions: readSessions(folder.uri.fsPath, folder.name),
  };
}

function readSessions(root: string, project: string): SessionInfo[] {
  const dir = path.join(root, '.duet', 'sessions');
  if (!fs.existsSync(dir)) {
    return [];
  }
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => {
      const full = path.join(dir, d.name);
      let state: SessionState | undefined;
      try {
        state = JSON.parse(fs.readFileSync(path.join(full, 'state.json'), 'utf8'));
      } catch {
        // in-progress or aborted session; no summary yet
      }
      return { kind: 'session' as const, dir: full, name: d.name, project, state };
    })
    .sort((a, b) => b.name.localeCompare(a.name));
}

/** Session dirs are named YYYYMMDD-HHMMSS-task-slug. */
function labelFor(el: SessionInfo): string {
  const slug = el.name.replace(/^\d{8}-\d{6}-?/, '');
  return slug.replace(/-/g, ' ') || el.name;
}

function descriptionFor(el: SessionInfo): string {
  const m = el.name.match(/^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})/);
  const time = m ? `${m[2]}/${m[3]} ${m[4]}:${m[5]}` : '';
  if (!el.state) {
    return time;
  }
  const verdict = el.state.success ? 'approved' : el.state.final_verdict ?? '';
  return [time, verdict].filter(Boolean).join(' · ');
}
