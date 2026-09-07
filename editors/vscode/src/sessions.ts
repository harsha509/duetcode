import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import { Activity } from './serveClient';

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
  /** The run in progress in this folder, listed ahead of its sessions. */
  running?: RunningNode;
}

/** The task or review in progress, shown until the server reports its outcome. */
export interface RunningNode extends Activity {
  kind: 'running';
}

export type SessionNode = ProjectNode | SessionInfo | RunningNode;

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

  /** `activity` reports what the server is running, or nothing when idle. */
  constructor(private readonly activity: () => Activity | undefined = () => undefined) {}

  refresh(): void {
    this._onDidChange.fire(undefined);
  }

  getTreeItem(el: SessionNode): vscode.TreeItem {
    switch (el.kind) {
      case 'project':
        return projectTreeItem(el);
      case 'running':
        return runningTreeItem(el);
      default:
        return sessionTreeItem(el);
    }
  }

  getChildren(el?: SessionNode): SessionNode[] {
    if (el) {
      return el.kind === 'project' ? withRunning(el.running, el.sessions) : [];
    }

    const running = runningNode(this.activity());
    const folders = vscode.workspace.workspaceFolders ?? [];
    const projects = folders
      .map((folder) => readProject(folder, running))
      .filter((project) => project.sessions.length > 0 || project.running);

    // One folder needs no grouping node — it would hold every session behind
    // an extra click and name a repo the user already knows.
    if (folders.length <= 1) {
      return withRunning(running, projects[0]?.sessions ?? []);
    }
    // A run in a folder outside the workspace still shows, ahead of the groups.
    const placed = projects.some((project) => project.running);
    return placed ? projects : withRunning(running, projects);
  }
}

function withRunning(running: RunningNode | undefined, rest: SessionNode[]): SessionNode[] {
  return running ? [running, ...rest] : rest;
}

/** The tree row for a run, labelled by its first line. */
function runningNode(activity: Activity | undefined): RunningNode | undefined {
  return activity && { kind: 'running', ...activity, label: activity.label.split('\n')[0] };
}

function runningTreeItem(el: RunningNode): vscode.TreeItem {
  const item = new vscode.TreeItem(el.label || 'task', vscode.TreeItemCollapsibleState.None);
  item.description = 'running…';
  item.tooltip = `${el.project}\n${el.label}`;
  item.iconPath = new vscode.ThemeIcon('loading~spin');
  item.contextValue = 'dtRunning';
  item.command = { command: 'dt.showPanel', title: 'Show Duet Panel' };
  return item;
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
  item.contextValue = 'dtSession';
  return item;
}

/** What a session is called in prompts and notifications. */
export function sessionLabel(el: SessionInfo): string {
  return el.state?.task ?? labelFor(el);
}

/** Every session in the workspace, across all folders. */
export function allSessions(): SessionInfo[] {
  return (vscode.workspace.workspaceFolders ?? []).flatMap((folder) => readProject(folder).sessions);
}

/**
 * Whether a directory is one of ours to delete. Commands take their argument
 * from the tree, but any extension can invoke one with a path of its choosing,
 * and a session is only ever a direct child of a folder's .duet/sessions.
 */
export function isSessionDir(dir: string): boolean {
  return (vscode.workspace.workspaceFolders ?? []).some(
    (folder) => path.dirname(dir) === path.join(folder.uri.fsPath, '.duet', 'sessions'),
  );
}

/**
 * Removes each session's logs from disk, returning the names of any that could
 * not be removed. One unwritable directory must not strand the rest of a clear,
 * and the removals are async so a large batch does not stall the extension host.
 */
export async function deleteSessions(sessions: SessionInfo[]): Promise<string[]> {
  const failed: string[] = [];
  for (const session of sessions) {
    if (!isSessionDir(session.dir)) {
      continue;
    }
    try {
      await fs.promises.rm(session.dir, { recursive: true, force: true });
    } catch {
      failed.push(sessionLabel(session));
    }
  }
  return failed;
}

function readProject(folder: vscode.WorkspaceFolder, running?: RunningNode): ProjectNode {
  const root = folder.uri.fsPath;
  const mine = running && sameDir(running.project, root) ? running : undefined;
  return {
    kind: 'project',
    name: folder.name,
    sessions: readSessions(root, folder.name, mine?.since),
    running: mine,
  };
}

function sameDir(a: string, b: string): boolean {
  return path.resolve(a) === path.resolve(b);
}

function readSessions(root: string, project: string, runningSince?: number): SessionInfo[] {
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
    .filter((session) => !isRunLog(session, runningSince))
    .sort((a, b) => b.name.localeCompare(a.name));
}

/**
 * Whether a session with no summary yet is the run in progress: the server
 * creates the directory once the run has started, so one stamped from that
 * moment on is the run's own log — already shown as the running row.
 */
function isRunLog(session: SessionInfo, runningSince: number | undefined): boolean {
  if (runningSince === undefined || session.state) {
    return false;
  }
  const stamped = sessionTime(session.name);
  return stamped !== undefined && stamped >= runningSince - 1000;
}

/** When the session started, from its directory name, as local epoch ms. */
function sessionTime(name: string): number | undefined {
  const m = name.match(/^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})/);
  return m ? new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]).getTime() : undefined;
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
