import * as vscode from 'vscode';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { ServeClient } from './serveClient';

/**
 * The duet webview: sessions history viewer + live task runner with
 * round-aligned writer/reviewer columns.
 */
export class DuetPanel {
  static current: DuetPanel | undefined;

  private pendingImages: string[] = [];
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

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    ctx: vscode.ExtensionContext,
    private readonly client: ServeClient,
  ) {
    panel.webview.html = this.renderHtml(ctx);
    this.disposables.push(
      this.client.onEvent((ev) => this.post({ type: 'event', ev })),
      this.client.onExit((code) => this.post({ type: 'serveExit', code })),
      panel.webview.onDidReceiveMessage((msg) => this.onMessage(msg)),
      panel.onDidDispose(() => this.dispose()),
    );
  }

  showHistory(sessionDir: string): void {
    this.panel.reveal();
    this.post({ type: 'history', data: readSession(sessionDir) });
  }

  private onMessage(msg: any): void {
    switch (msg.type) {
      case 'task': {
        const cmd: Record<string, unknown> = {
          cmd: msg.plan ? 'plan' : 'task',
          task: msg.text,
          auto: !!msg.auto,
        };
        if (this.pendingImages.length > 0) {
          cmd.images = this.pendingImages;
          this.pendingImages = [];
        }
        this.client.send(cmd);
        break;
      }
      case 'review':
        this.client.send({ cmd: 'review', task: msg.text || undefined });
        break;
      case 'answer':
        this.client.send({ cmd: 'answer', id: msg.id, value: msg.value });
        break;
      case 'attach':
        void this.pickImages();
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
    const file = path.join(os.tmpdir(), `dt-paste-${Date.now()}.png`);
    fs.writeFileSync(file, Buffer.from(dataB64, 'base64'));
    this.pendingImages.push(file);
    this.post({ type: 'attached', name: 'clipboard image' });
  }

  private post(msg: unknown): void {
    void this.panel.webview.postMessage(msg);
  }

  private renderHtml(ctx: vscode.ExtensionContext): string {
    const webview = this.panel.webview;
    const js = webview.asWebviewUri(vscode.Uri.joinPath(ctx.extensionUri, 'media', 'main.js'));
    const css = webview.asWebviewUri(vscode.Uri.joinPath(ctx.extensionUri, 'media', 'main.css'));
    const nonce = Math.random().toString(36).slice(2);
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
      <label><input type="checkbox" id="auto" checked> auto</label>
      <label><input type="checkbox" id="plan"> plan</label>
      <button id="attach" title="Attach image">📎</button>
      <button id="review" title="Review uncommitted changes">review</button>
      <button id="send">Send</button>
    </div>
  </footer>
  <script nonce="${nonce}" src="${js}"></script>
</body>
</html>`;
  }

  private dispose(): void {
    DuetPanel.current = undefined;
    for (const d of this.disposables) {
      d.dispose();
    }
  }
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
    rounds,
  };
}
