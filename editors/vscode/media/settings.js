// @ts-check
(function () {
  const vscode = acquireVsCodeApi();

  const $ = (id) => document.getElementById(id);
  const writer = $('writer');
  const claudeModel = $('claudeModel');
  const geminiModel = $('geminiModel');
  const anthropicKey = $('anthropicKey');
  const geminiKey = $('geminiKey');
  const binaryPath = $('binaryPath');
  const savedNote = $('savedNote');
  const refreshModels = $('refreshModels');
  const modelsStatus = $('modelsStatus');

  function setKeyStatus(el, stored) {
    el.textContent = stored ? 'stored ✓' : 'not set';
    el.classList.toggle('stored', stored);
  }

  function showStatus(text) {
    modelsStatus.textContent = text;
    modelsStatus.classList.toggle('hidden', !text);
  }

  /** Replace a datalist's options with the fetched model ids. */
  function fillDatalist(id, values) {
    const dl = $(id);
    if (!dl || !values || !values.length) {
      return;
    }
    dl.replaceChildren(
      ...values.map((v) => {
        const opt = document.createElement('option');
        opt.value = v;
        return opt;
      }),
    );
  }

  window.addEventListener('message', (e) => {
    const msg = e.data;
    if (msg.type === 'state') {
      writer.value = msg.state.writer;
      claudeModel.value = msg.state.claudeModel;
      geminiModel.value = msg.state.geminiModel;
      binaryPath.value = msg.state.binaryPath === 'dt' ? '' : msg.state.binaryPath;
      setKeyStatus($('anthropicKeyStatus'), msg.state.hasAnthropicKey);
      setKeyStatus($('geminiKeyStatus'), msg.state.hasGeminiKey);
    } else if (msg.type === 'saved') {
      anthropicKey.value = '';
      geminiKey.value = '';
      savedNote.classList.remove('hidden');
      setTimeout(() => savedNote.classList.add('hidden'), 3000);
    } else if (msg.type === 'models') {
      refreshModels.disabled = false;
      fillDatalist('geminiModels', msg.gemini);
      fillDatalist('claudeModels', msg.claude);
      showStatus(msg.note);
    }
  });

  refreshModels.addEventListener('click', () => {
    refreshModels.disabled = true;
    showStatus('Fetching latest models…');
    vscode.postMessage({ type: 'refreshModels' });
  });

  $('save').addEventListener('click', () => {
    vscode.postMessage({
      type: 'save',
      values: {
        writer: writer.value,
        claudeModel: claudeModel.value,
        geminiModel: geminiModel.value,
        anthropicKey: anthropicKey.value,
        geminiKey: geminiKey.value,
        binaryPath: binaryPath.value,
      },
    });
  });

  $('clearAnthropicKey').addEventListener('click', () => {
    vscode.postMessage({ type: 'clearKey', key: 'anthropic' });
  });
  $('clearGeminiKey').addEventListener('click', () => {
    vscode.postMessage({ type: 'clearKey', key: 'gemini' });
  });
  $('checkClaude').addEventListener('click', () => {
    vscode.postMessage({ type: 'checkClaude' });
  });
})();
