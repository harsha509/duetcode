import * as vscode from 'vscode';
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { ServeClient } from './serveClient';

/**
 * Composer icons, drawn rather than typed.
 *
 * `📎` and `⚙` disagree about themselves: the paperclip defaults to emoji
 * presentation and arrives from a colour font at roughly 1.3em, while the gear
 * defaults to text presentation and falls back to whatever monochrome symbol
 * font the platform has — smaller, differently baselined, and a different
 * weight. Matching them is not possible in CSS because the discrepancy is in
 * the fonts. Geometry that scales with the button removes the question.
 *
 * Stroke and fill are attributes, not inline styles: the webview's CSP allows
 * no inline `style`, and `currentColor` lets the button's own colour through.
 */
const ICON_SVG_ATTRS =
  'viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
  'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"';

const ICON_ATTACH =
  `<svg ${ICON_SVG_ATTRS}><path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 ` +
  `5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>`;

const ICON_SETTINGS =
  `<svg ${ICON_SVG_ATTRS}><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82` +
  `l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 ` +
  `1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06` +
  `a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 ` +
  `2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 ` +
  `0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 ` +
  `2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 ` +
  `0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 ` +
  `1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>`;

/**
 * The duet webview: sessions history viewer + live task runner with
 * round-aligned writer/reviewer columns.
 */
export class DuetPanel {
  static current: DuetPanel | undefined;

  private pendingImages: string[] = [];
  /** Pasted screenshots written to tmp for the current task; deleted when it ends. */
  private sentTmpImages: string[] = [];
  private readonly disposables: vscode.Disposable[] = [];

  static createOrShow(ctx: vscode.ExtensionContext, client: ServeClient): DuetPanel {
    if (DuetPanel.current) {
      DuetPanel.current.panel.reveal();
      return DuetPanel.current;
    }
    const panel = vscode.window.createWebviewPanel(
      'dtDuet',
      'DT Duet',
      vscode.ViewColumn.Beside,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [vscode.Uri.joinPath(ctx.extensionUri, 'media')],
      },
    );
    DuetPanel.current = new DuetPanel(panel, ctx, client);
    return DuetPanel.current;
  }

  /** Closes the open panel, if there is one. A no-op otherwise. */
  static close(): void {
    DuetPanel.current?.panel.dispose();
  }

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    ctx: vscode.ExtensionContext,
    private readonly client: ServeClient,
  ) {
    panel.webview.html = this.renderHtml(ctx);
    this.disposables.push(
      this.client.onEvent((ev) => {
        this.post({ type: 'event', ev });
        if (ev.event === 'task_done' || ev.event === 'error') {
          this.cleanupTmpImages();
        }
      }),
      this.client.onExit((code) => this.post({ type: 'serveExit', code })),
      panel.webview.onDidReceiveMessage((msg) => this.onMessage(msg)),
      panel.onDidDispose(() => this.dispose()),
      // The picker must track folders added or removed after the panel opened.
      vscode.workspace.onDidChangeWorkspaceFolders(() => this.postProjects()),
    );
    this.postProjects();
  }

  private postProjects(): void {
    this.post({
      type: 'projects',
      projects: (vscode.workspace.workspaceFolders ?? []).map((folder) => ({
        name: folder.name,
        path: folder.uri.fsPath,
      })),
    });
  }

  showHistory(sessionDir: string): void {
    this.panel.reveal();
    this.post({ type: 'history', data: readSession(sessionDir) });
  }

  private onMessage(msg: any): void {
    switch (msg.type) {
      case 'ready':
        // The webview finished loading; replay the list it may have missed.
        this.postProjects();
        break;
      case 'task': {
        const cmd: Record<string, unknown> = {
          cmd: msg.plan ? 'plan' : 'task',
          task: msg.text,
          auto: !!msg.auto,
          // Set by the composer's project picker, which follows the project of
          // the last review — so a fix lands where the findings are.
          dir: msg.dir || undefined,
        };
        if (this.pendingImages.length > 0) {
          cmd.images = this.pendingImages;
          const tmp = os.tmpdir();
          this.sentTmpImages.push(...this.pendingImages.filter((p) => p.startsWith(tmp)));
          this.pendingImages = [];
        }
        this.client.send(cmd);
        break;
      }
      case 'review':
        this.client.send({
          cmd: 'review',
          task: msg.text || undefined,
          dirs: workspaceProjectDirs(),
        });
        break;
      case 'answer':
        this.client.send({ cmd: 'answer', id: msg.id, value: msg.value });
        break;
      case 'attach':
        void this.pickImages();
        break;
      case 'settings':
        void vscode.commands.executeCommand('dt.settings');
        break;
      case 'pastedImage':
        this.savePastedImage(msg.dataB64);
        break;
      case 'openFile':
        void vscode.workspace
          .openTextDocument(msg.path)
          .then((doc) => vscode.window.showTextDocument(doc, { preview: true }));
        break;
    }
  }

  private async pickImages(): Promise<void> {
    const uris = await vscode.window.showOpenDialog({
      canSelectMany: true,
      filters: { Images: ['png', 'jpg', 'jpeg', 'gif', 'webp'] },
    });
    for (const uri of uris ?? []) {
      this.pendingImages.push(uri.fsPath);
      this.post({ type: 'attached', name: path.basename(uri.fsPath) });
    }
  }

  private savePastedImage(dataB64: string): void {
    const file = path.join(os.tmpdir(), `dt-paste-${crypto.randomUUID()}.png`);
    fs.writeFileSync(file, Buffer.from(dataB64, 'base64'), { flag: 'wx' });
    this.pendingImages.push(file);
    this.post({ type: 'attached', name: 'clipboard image' });
  }

  private cleanupTmpImages(): void {
    for (const file of this.sentTmpImages.splice(0)) {
      fs.unlink(file, () => {});
    }
  }

  private post(msg: unknown): void {
    void this.panel.webview.postMessage(msg);
  }

  private renderHtml(ctx: vscode.ExtensionContext): string {
    const webview = this.panel.webview;
    const js = webview.asWebviewUri(vscode.Uri.joinPath(ctx.extensionUri, 'media', 'main.js'));
    const css = webview.asWebviewUri(vscode.Uri.joinPath(ctx.extensionUri, 'media', 'main.css'));
    const nonce = crypto.randomBytes(16).toString('base64');
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy"
        content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
  <link rel="stylesheet" href="${css}">
  <title>DT Duet</title>
</head>
<body>
  <header id="header">
    <span id="title">DT Duet</span>
    <span id="models"></span>
    <span id="status"></span>
  </header>
  <main id="timeline"></main>
  <div id="askbar" class="hidden"></div>
  <footer id="composer">
    <div id="chips"></div>
    <textarea id="input" rows="2" placeholder="Describe a task for the duet…  (paste screenshots directly)"></textarea>
    <div id="controls">
      <select id="project" class="hidden" title="Project a task runs in"></select>
      <label><input type="checkbox" id="auto" checked> auto</label>
      <label><input type="checkbox" id="plan"> plan</label>
      <button id="attach" class="icon" title="Attach image" aria-label="Attach image">${ICON_ATTACH}</button>
      <button id="settings" class="icon" title="Settings — API keys, Claude login" aria-label="Settings">${ICON_SETTINGS}</button>
      <button id="review" title="Second opinion on the last answer, or on the uncommitted changes, in every workspace project">review</button>
      <button id="send">Send</button>
    </div>
  </footer>
  <script nonce="${nonce}" src="${js}"></script>
</body>
</html>`;
  }

  private dispose(): void {
    DuetPanel.current = undefined;
    // A panel closed mid-task never sees task_done, so the screenshots it
    // wrote to tmp are swept here instead of leaking one file per paste.
    this.cleanupTmpImages();
    for (const d of this.disposables) {
      d.dispose();
    }
  }
}

/**
 * Every project in the workspace, so a review covers all of them instead of
 * only the folder `dt serve` was spawned in. Read at send time, so folders
 * added after activation are included.
 */
function workspaceProjectDirs(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).map((folder) => folder.uri.fsPath);
}

interface RoundData {
  round: number;
  writer?: string;
  reviewer?: string;
  checks?: unknown;
  patchPath?: string;
  clarification?: string;
}

/**
 * Which model held which role in a stored session, from the session's own
 * record — never from the current configuration, which would relabel finished
 * work every time the writer is switched.
 *
 * roles.json is written when a session is created. state.json is the fallback
 * for sessions recorded before it existed, and only runs that reached a summary
 * have one. Sessions with neither are left undefined: nothing on disk says who
 * ran them, and a guess would read exactly like a fact.
 */
function readRoles(dir: string, state: unknown): { writer?: string; reviewer?: string } {
  const names = (source: unknown) => {
    const record = source as { writer?: unknown; reviewer?: unknown } | null;
    const writer = typeof record?.writer === 'string' ? record.writer : undefined;
    const reviewer = typeof record?.reviewer === 'string' ? record.reviewer : undefined;
    return writer && reviewer ? { writer, reviewer } : undefined;
  };

  let recorded: unknown;
  try {
    recorded = JSON.parse(fs.readFileSync(path.join(dir, 'roles.json'), 'utf8'));
  } catch {
    recorded = null;
  }

  return names(recorded) ?? names(state) ?? {};
}

/**
 * Load a stored session for the history view. Log filenames are fixed:
 * claude_out.md is always the writer's output and gemini_out.md the
 * reviewer's, regardless of which model held which role.
 */
function readSession(dir: string) {
  const read = (p: string): string | undefined => {
    try {
      return fs.readFileSync(p, 'utf8');
    } catch {
      return undefined;
    }
  };

  let state: unknown;
  try {
    state = JSON.parse(read(path.join(dir, 'state.json')) ?? 'null');
  } catch {
    state = null;
  }

  const rounds: RoundData[] = [];
  for (let i = 0; i <= 20; i++) {
    const roundDir = path.join(dir, `round-${i}`);
    if (!fs.existsSync(roundDir)) {
      continue;
    }
    const patch = path.join(roundDir, 'claude.patch');
    let checks: unknown;
    try {
      checks = JSON.parse(read(path.join(roundDir, 'checks.json')) ?? 'null');
    } catch {
      checks = null;
    }
    rounds.push({
      round: i,
      writer: read(path.join(roundDir, 'claude_out.md')),
      reviewer: read(path.join(roundDir, 'gemini_out.md')),
      checks,
      patchPath: fs.existsSync(patch) ? patch : undefined,
      clarification: read(path.join(roundDir, 'clarification.md')),
    });
  }

  return {
    name: path.basename(dir),
    task: read(path.join(dir, 'prompt.md')) ?? '',
    state,
    roles: readRoles(dir, state),
    rounds,
  };
}
