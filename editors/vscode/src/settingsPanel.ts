import * as vscode from 'vscode';
import { ServeClient } from './serveClient';
import { SECRET_ANTHROPIC, SECRET_GEMINI } from './settings';
import { fetchClaudeModels, fetchGeminiModels } from './modelCatalog';

/**
 * Full-page settings webview (Cline-style form) replacing the old QuickPick:
 * model choices, API keys, and advanced options in one place with Save.
 */
export class SettingsPanel {
  static current: SettingsPanel | undefined;

  private readonly disposables: vscode.Disposable[] = [];

  static createOrShow(ctx: vscode.ExtensionContext, client: ServeClient): SettingsPanel {
    if (SettingsPanel.current) {
      SettingsPanel.current.panel.reveal();
      void SettingsPanel.current.sendState();
      return SettingsPanel.current;
    }
    const panel = vscode.window.createWebviewPanel(
      'dtDuetSettings',
      'DT Duet Settings',
      vscode.ViewColumn.Active,
      {
        enableScripts: true,
        localResourceRoots: [vscode.Uri.joinPath(ctx.extensionUri, 'media')],
      },
    );
    SettingsPanel.current = new SettingsPanel(panel, ctx, client);
    return SettingsPanel.current;
  }

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    private readonly ctx: vscode.ExtensionContext,
    private readonly client: ServeClient,
  ) {
    panel.webview.html = this.renderHtml(ctx);
    this.disposables.push(
      panel.webview.onDidReceiveMessage((msg) => void this.onMessage(msg)),
      panel.onDidDispose(() => this.dispose()),
    );
    void this.sendState();
  }

  /** Push current settings into the form. Secret values never leave the
   *  keychain — only a stored/not-set flag is sent. */
  private async sendState(): Promise<void> {
    const cfg = vscode.workspace.getConfiguration('dt');
    await this.panel.webview.postMessage({
      type: 'state',
      state: {
        binaryPath: cfg.get('binaryPath', 'dt'),
        writer: cfg.get('writer', 'claude'),
        claudeModel: cfg.get('claudeModel', ''),
        geminiModel: cfg.get('geminiModel', ''),
        hasAnthropicKey: !!(await this.ctx.secrets.get(SECRET_ANTHROPIC)),
        hasGeminiKey: !!(await this.ctx.secrets.get(SECRET_GEMINI)),
      },
    });
  }

  private async onMessage(msg: any): Promise<void> {
    switch (msg.type) {
      case 'save':
        await this.save(msg.values ?? {});
        break;
      case 'clearKey':
        await this.ctx.secrets.delete(msg.key === 'anthropic' ? SECRET_ANTHROPIC : SECRET_GEMINI);
        this.client.restart();
        await this.sendState();
        break;
      case 'checkClaude': {
        const term = vscode.window.createTerminal({ name: 'Claude CLI' });
        term.show();
        term.sendText('claude auth status --text', true);
        void vscode.window.showInformationMessage(
          'If not authenticated, run `claude` in this terminal and use /login.',
        );
        break;
      }
      case 'refreshModels':
        await this.refreshModels();
        break;
    }
  }

  /** Fetch the latest models from each provider using the stored keys and push
   *  them into the form's datalists. Claude only refreshes when an Anthropic
   *  API key is stored (the CLI can't list models); Gemini uses its key. */
  private async refreshModels(): Promise<void> {
    const [anthropicKey, geminiKey] = await Promise.all([
      this.ctx.secrets.get(SECRET_ANTHROPIC),
      this.ctx.secrets.get(SECRET_GEMINI),
    ]);

    const [gemini, claude] = await Promise.all([
      SettingsPanel.discover('Gemini', geminiKey, fetchGeminiModels,
        'add a Gemini API key above to refresh'),
      SettingsPanel.discover('Claude', anthropicKey, fetchClaudeModels,
        'CLI mode tracks the sonnet/opus/haiku aliases — run `claude update`; add an Anthropic API key to list full ids'),
    ]);

    await this.panel.webview.postMessage({
      type: 'models',
      gemini: gemini.models,
      claude: claude.models,
      note: `${gemini.note}  ·  ${claude.note}`,
    });
  }

  /** Fetch one provider's models, mapping the no-key and failure cases to a
   *  human-readable note rather than throwing. */
  private static async discover(
    label: string,
    apiKey: string | undefined,
    fetchModels: (key: string) => Promise<string[]>,
    noKeyHint: string,
  ): Promise<{ models?: string[]; note: string }> {
    if (!apiKey) {
      return { note: `${label}: ${noKeyHint}` };
    }
    try {
      const models = await fetchModels(apiKey);
      return { models, note: `${label}: ${models.length} models` };
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      return { note: `${label}: refresh failed — ${message}` };
    }
  }

  private async save(values: any): Promise<void> {
    const cfg = vscode.workspace.getConfiguration('dt');
    const target = vscode.ConfigurationTarget.Global;
    const norm = (v: unknown): string | undefined =>
      typeof v === 'string' && v.trim() !== '' ? v.trim() : undefined;

    await cfg.update('writer', values.writer === 'gemini' ? 'gemini' : 'claude', target);
    await cfg.update('claudeModel', norm(values.claudeModel), target);
    await cfg.update('geminiModel', norm(values.geminiModel), target);
    await cfg.update('binaryPath', norm(values.binaryPath), target);

    // Empty key fields mean "keep the stored value"; clearing has its own button.
    const anthropic = norm(values.anthropicKey);
    if (anthropic) {
      await this.ctx.secrets.store(SECRET_ANTHROPIC, anthropic);
    }
    const gemini = norm(values.geminiKey);
    if (gemini) {
      await this.ctx.secrets.store(SECRET_GEMINI, gemini);
    }

    this.client.restart();
    await this.sendState();
    await this.panel.webview.postMessage({ type: 'saved' });
  }

  private renderHtml(ctx: vscode.ExtensionContext): string {
    const webview = this.panel.webview;
    const js = webview.asWebviewUri(vscode.Uri.joinPath(ctx.extensionUri, 'media', 'settings.js'));
    const css = webview.asWebviewUri(vscode.Uri.joinPath(ctx.extensionUri, 'media', 'settings.css'));
    const nonce = Math.random().toString(36).slice(2);
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy"
        content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
  <link rel="stylesheet" href="${css}">
  <title>DT Duet Settings</title>
</head>
<body>
  <h1>DT Duet Settings</h1>

  <section>
    <div class="section-head">
      <h2>Models</h2>
      <button id="refreshModels" class="link" title="Fetch the latest available models from each provider">⟳ Refresh</button>
    </div>
    <p id="modelsStatus" class="hint hidden"></p>

    <div class="field">
      <label for="writer">Writer</label>
      <select id="writer">
        <option value="claude">Claude writes, Gemini reviews</option>
        <option value="gemini">Gemini writes, Claude reviews</option>
      </select>
      <p class="hint">Which model writes code — the other one reviews it.</p>
    </div>

    <div class="field">
      <label for="claudeModel">Claude model</label>
      <input id="claudeModel" type="text" list="claudeModels" placeholder="sonnet (default)">
      <datalist id="claudeModels">
        <option value="sonnet"></option>
        <option value="opus"></option>
        <option value="haiku"></option>
        <option value="claude-opus-4-8"></option>
        <option value="claude-sonnet-5"></option>
      </datalist>
      <p class="hint">Alias like "sonnet" / "opus", or a full model id. Leave empty to use
        .duet/config.toml (default: sonnet). Use a full id if you rely on Claude API mode.</p>
    </div>

    <div class="field">
      <label for="geminiModel">Gemini model</label>
      <input id="geminiModel" type="text" list="geminiModels" placeholder="gemini-3.1-pro-preview (default)">
      <datalist id="geminiModels">
        <option value="gemini-3.1-pro-preview"></option>
        <option value="gemini-2.5-pro"></option>
        <option value="gemini-2.5-flash"></option>
      </datalist>
      <p class="hint">Leave empty to use .duet/config.toml (default: gemini-3.1-pro-preview).</p>
    </div>
  </section>

  <section>
    <h2>API Keys</h2>

    <div class="field">
      <label for="geminiKey">Gemini API key <span id="geminiKeyStatus" class="badge"></span></label>
      <div class="row">
        <input id="geminiKey" type="password" placeholder="AIza…" autocomplete="off">
        <button id="clearGeminiKey" class="secondary" title="Remove the stored key">Clear</button>
      </div>
      <p class="hint">Stored in your OS keychain, injected as GEMINI_API_KEY for dt serve.
        Leave empty to keep the stored key.</p>
    </div>

    <div class="field">
      <label for="anthropicKey">Anthropic API key <span id="anthropicKeyStatus" class="badge"></span></label>
      <div class="row">
        <input id="anthropicKey" type="password" placeholder="sk-ant-…" autocomplete="off">
        <button id="clearAnthropicKey" class="secondary" title="Remove the stored key">Clear</button>
      </div>
      <p class="hint">Optional — only needed for Claude API mode or as CLI fallback.
        Leave empty to keep the stored key.</p>
    </div>

    <div class="field">
      <label>Claude CLI</label>
      <div class="row">
        <button id="checkClaude" class="secondary">Check auth / login</button>
      </div>
      <p class="hint">Opens a terminal with the auth status; run <code>claude</code> and /login if needed.</p>
    </div>
  </section>

  <section>
    <h2>Advanced</h2>

    <div class="field">
      <label for="binaryPath">dt binary path</label>
      <input id="binaryPath" type="text" placeholder="dt">
      <p class="hint">Path to the dt binary. Leave empty to use "dt" from PATH.</p>
    </div>
  </section>

  <footer>
    <button id="save">Save</button>
    <span id="savedNote" class="hidden">Saved ✓ — applies from the next task.</span>
  </footer>

  <script nonce="${nonce}" src="${js}"></script>
</body>
</html>`;
  }

  private dispose(): void {
    SettingsPanel.current = undefined;
    for (const d of this.disposables) {
      d.dispose();
    }
  }
}
