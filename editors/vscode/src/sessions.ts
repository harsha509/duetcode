import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';

export interface SessionInfo {
  dir: string;
  name: string;
  state?: SessionState;
}

interface SessionState {
  task?: string;
  success?: boolean;
  final_verdict?: string;
  total_rounds?: number;
}

/** Sidebar tree of past duet sessions, read straight from .duet/sessions. */
export class SessionsProvider implements vscode.TreeDataProvider<SessionInfo> {
  private readonly _onDidChange = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChange.event;

  constructor(private readonly workspaceRoot: string | undefined) {}

  refresh(): void {
    this._onDidChange.fire();
  }

  getTreeItem(el: SessionInfo): vscode.TreeItem {
    const item = new vscode.TreeItem(labelFor(el), vscode.TreeItemCollapsibleState.None);
    item.description = descriptionFor(el);
    item.tooltip = el.state?.task ?? el.name;
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

  getChildren(): SessionInfo[] {
    if (!this.workspaceRoot) {
      return [];
    }
    const dir = path.join(this.workspaceRoot, '.duet', 'sessions');
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
        return { dir: full, name: d.name, state };
      })
      .sort((a, b) => b.name.localeCompare(a.name));
  }
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
