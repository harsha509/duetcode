// Webview logic: renders live duet events and stored session history into
// round-aligned writer/reviewer columns.
(function () {
  const vscode = acquireVsCodeApi();

  const timeline = document.getElementById('timeline');
  const modelsEl = document.getElementById('models');
  const statusEl = document.getElementById('status');
  const askbar = document.getElementById('askbar');
  const chips = document.getElementById('chips');
  const input = document.getElementById('input');
  const autoBox = document.getElementById('auto');
  const planBox = document.getElementById('plan');
  const projectSel = document.getElementById('project');

  let writerName = 'claude';
  let reviewerName = 'gemini';
  let currentRound = null; // { writerCol, reviewerCol }
  let streams = {}; // model -> <pre> currently receiving chunks
  let busy = false;

  // ── helpers ────────────────────────────────────────────────

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function scrollDown() {
    timeline.scrollTop = timeline.scrollHeight;
  }

  function sideFor(actor) {
    if (actor === writerName) return 'writer';
    return 'reviewer'; // reviewer, checks, and everything review-adjacent
  }

  // Columns are created on first use, so a round only shows the blocks that
  // actually have content (no empty reviewer box while the writer streams).
  function colFor(actor) {
    if (!currentRound) newRound('•', '');
    const side = sideFor(actor);
    const key = side + 'Col';
    if (!currentRound[key]) {
      const col = el('div', 'col ' + side);
      const label = side === 'writer' ? writerName + ' · writer' : reviewerName + ' · reviewer';
      col.appendChild(el('div', 'col-head', label));
      currentRound.cols.appendChild(col);
      currentRound[key] = col;
    }
    return currentRound[key];
  }

  function line(target, cls, text) {
    target.appendChild(el('div', cls, text));
    scrollDown();
  }

  function fullWidth(cls, text) {
    line(timeline, 'row ' + cls, text);
  }

  function newRound(label, budget) {
    const block = el('section', 'round');
    const head = el('div', 'round-head', budget ? `round ${label}/${budget}` : String(label));
    const cols = el('div', 'cols');
    block.appendChild(head);
    block.appendChild(cols);
    timeline.appendChild(block);
    currentRound = { cols, writerCol: null, reviewerCol: null };
    streams = {};
    scrollDown();
  }

  function setBusy(b) {
    busy = b;
    document.getElementById('send').disabled = b;
    statusEl.textContent = b ? 'running…' : '';
  }

  function showAsk(id, kind, question) {
    askbar.innerHTML = '';
    askbar.classList.remove('hidden');
    askbar.appendChild(el('span', 'ask-q', question));
    if (kind === 'yes_no') {
      const yes = el('button', 'primary', 'Yes');
      const no = el('button', '', 'No');
      yes.onclick = () => answer(id, 'y');
      no.onclick = () => answer(id, 'n');
      askbar.appendChild(yes);
      askbar.appendChild(no);
    } else {
      const field = el('input');
      field.type = 'text';
      field.placeholder = 'your guidance… (empty stops the task)';
      const send = el('button', 'primary', 'Send');
      send.onclick = () => answer(id, field.value);
      field.onkeydown = (e) => {
        if (e.key === 'Enter') answer(id, field.value);
      };
      askbar.appendChild(field);
      askbar.appendChild(send);
      field.focus();
    }
  }

  function answer(id, value) {
    askbar.classList.add('hidden');
    askbar.innerHTML = '';
    vscode.postMessage({ type: 'answer', id, value });
  }

  function renderVerdict(target, approved, blockers, suggestions) {
    const chip = el('div', 'verdict ' + (approved ? 'ok' : 'bad'),
      approved ? 'APPROVED' : 'CHANGES REQUESTED');
    target.appendChild(chip);
    for (const b of blockers || []) line(target, 'blocker', '✗ ' + b);
    for (const s of suggestions || []) line(target, 'suggestion', '~ ' + s);
    scrollDown();
  }

  // ── severity colouring ─────────────────────────────────────

  // A model's prose arrives as one long block and reads as a wall. Only lines
  // that *announce* something are tinted — a heading, or a list item with a
  // bold lead-in, the shapes reviewers use to label a finding — so the colour
  // marks findings instead of shouting every sentence that says "error".
  // Order matters: the clearing patterns are tested first, so "no bugs found"
  // is not filed under bugs.
  const SEVERITY = [
    ['ok', /\bno\s+(?:known\s+)?(?:bugs?|issues?|blockers?|problems?|findings?)\b|\bverified\b|\bno action\b|\blooks good\b|\bapproved\b|\ball (?:checks )?pass(?:ed|ing)?\b|✓/i],
    ['issue', /\bblock(?:ing|er|ers|ed)\b|\bcritical\b|\bmust fix\b|\bbugs?\b|\bbroken\b|\bregressions?\b|\bsecurity\b|\bcrash(?:es|ing)?\b|\bdata loss\b|\bfail(?:s|ed|ure|ing)?\b|\bchanges[ _]requested\b|\brequest changes\b|\bincorrect\b|✗/i],
    ['warn', /\bshould[- ]fix\b|\bwarn(?:ing)?\b|\bcaution\b|\brisks?\b|\bminor\b|\bnit\b|\bconsider\b|\bsuggestions?\b|\bscope creep\b|\bstale\b|\bunverified\b|\bmissing\b/i],
  ];

  function severityOf(text) {
    for (const [cls, re] of SEVERITY) {
      if (re.test(text)) {
        return cls;
      }
    }
    return '';
  }

  /** Heading level, or 0 for anything else. */
  function headingLevel(line) {
    const m = /^\s{0,3}(#{1,6})\s/.exec(line);
    return m ? m[1].length : 0;
  }

  /** A list item whose lead-in is bold — how reviewers label a finding. */
  function isFinding(line) {
    return /^\s*(?:[-*+]|\d+[.)])\s+\*\*/.test(line);
  }

  /**
   * Repaints a block of prose, one span per line, tinted by severity.
   *
   * A finding is usually named under its section rather than in its own words
   * ("### 1. GZip compression was silently deleted" under "## Blocking"), so a
   * classified heading lends its severity to everything nested beneath it. The
   * section ends at the next heading of the same or shallower level, which is
   * where the reviewer stopped talking about it.
   *
   * Inheritance escalates and never clears: `ok` is not handed down, because a
   * line painted green by its neighbours is a claim that there is nothing to do
   * here — the one claim this must never invent. A clean-sounding heading over
   * a list of problems ("Key Highlights Verified") leaves those items plain.
   */
  function fillProse(pre, text) {
    pre.textContent = '';
    let section = null; // { level, severity } of the enclosing tinted heading

    const inherited = () => (section && section.severity !== 'ok' ? section.severity : '');

    for (const raw of text.split('\n')) {
      const level = headingLevel(raw);
      let severity = '';

      if (level > 0) {
        severity = severityOf(raw);
        if (severity) {
          section = { level, severity };
        } else if (section && level > section.level) {
          severity = inherited();
        } else {
          section = null;
        }
      } else if (isFinding(raw)) {
        severity = severityOf(raw) || inherited();
      }

      pre.appendChild(el('span', severity ? 'pl ' + severity : 'pl', raw + '\n'));
    }
  }

  function renderProse(target, cls, text) {
    const pre = el('pre', cls);
    fillProse(pre, text);
    target.appendChild(pre);
    scrollDown();
  }

  // ── live events ────────────────────────────────────────────

  function onEvent(ev) {
    switch (ev.event) {
      case 'ready':
        writerName = ev.writer;
        reviewerName = ev.reviewer;
        modelsEl.textContent = `${ev.writer} writes · ${ev.reviewer} reviews`;
        break;
      case 'task_started': {
        writerName = ev.writer;
        reviewerName = ev.reviewer;
        currentRound = null;
        const head = el('div', 'task-head');
        head.appendChild(el('span', 'task-title', ev.task));
        head.appendChild(el('span', 'task-mode', `${ev.mode} · max ${ev.max_rounds} rounds`));
        timeline.appendChild(head);
        setBusy(true);
        break;
      }
      case 'round_started':
        newRound(ev.round, ev.budget);
        break;
      case 'section':
        currentRound = null;
        fullWidth('section', '— ' + ev.title + ' —');
        break;
      case 'project_started':
        currentRound = null;
        // Point the composer at the project being reviewed, so a fix typed
        // straight after the review runs where the findings actually are.
        selectProject(ev.path);
        fullWidth('project', '📁 ' + ev.name + ' — ' + ev.path);
        break;
      case 'working':
        line(colFor(ev.actor), 'working', '● ' + ev.actor + ' — ' + ev.action);
        break;
      case 'thinking':
        line(colFor(ev.model), 'dim', '◌ thinking…');
        break;
      case 'tool_action':
        line(colFor(ev.model), 'tool', '⚡ ' + ev.desc);
        break;
      case 'stream_start': {
        const pre = el('pre', 'stream');
        colFor(ev.model).appendChild(pre);
        streams[ev.model] = pre;
        break;
      }
      case 'stream_chunk': {
        let pre = streams[ev.model];
        if (!pre) {
          pre = el('pre', 'stream');
          colFor(ev.model).appendChild(pre);
          streams[ev.model] = pre;
        }
        pre.textContent += ev.text;
        scrollDown();
        break;
      }
      case 'stream_end': {
        // Tinted once the text is whole: a chunk can split a line anywhere, so
        // classifying mid-stream would colour half a heading.
        const pre = streams[ev.model];
        if (pre) {
          fillProse(pre, pre.textContent);
        }
        delete streams[ev.model];
        break;
      }
      case 'response':
        renderProse(colFor(ev.model), 'stream', ev.text);
        break;
      case 'check':
        line(colFor(reviewerName), ev.passed ? 'check ok' : 'check bad',
          (ev.passed ? '✓ ' : '✗ ') + ev.name);
        break;
      case 'verdict':
        renderVerdict(colFor(reviewerName), ev.approved, ev.blockers, ev.suggestions);
        break;
      case 'changes':
        fullWidth('changes', ev.stat.trim());
        break;
      case 'usage':
        statusEl.textContent =
          `${ev.model}: ${ev.input_tokens}in/${ev.output_tokens}out` +
          (ev.cost_usd ? ` $${ev.cost_usd.toFixed(4)}` : '');
        break;
      case 'cost_summary':
        fullWidth('cost',
          `${ev.calls} calls · ${ev.input_tokens + ev.output_tokens} tokens` +
          (ev.cost_usd ? ` · $${ev.cost_usd.toFixed(4)}` : ''));
        break;
      case 'info':
        fullWidth('info', 'ℹ ' + ev.text);
        break;
      case 'warn':
        fullWidth('warn', '⚠ ' + ev.text);
        break;
      case 'blocker':
        fullWidth('warn', '✗ ' + ev.text);
        break;
      case 'success':
        fullWidth('success', ev.text);
        break;
      case 'stopped':
        fullWidth('warn', ev.text);
        break;
      case 'ask':
        showAsk(ev.id, ev.kind, ev.question);
        break;
      case 'task_done':
        fullWidth(ev.success ? 'success' : 'warn',
          `${ev.success ? 'SUCCESS' : 'STOPPED'} — ${ev.message} (${ev.rounds} rounds)`);
        setBusy(false);
        break;
      case 'error':
        fullWidth('error', '✗ ' + ev.message);
        setBusy(false);
        break;
    }
  }

  // ── history rendering ──────────────────────────────────────

  function renderHistory(data) {
    timeline.innerHTML = '';
    currentRound = null;
    const head = el('div', 'task-head');
    head.appendChild(el('span', 'task-title', data.task));
    if (data.state) {
      head.appendChild(el('span', 'task-mode',
        (data.state.success ? 'approved' : data.state.final_verdict || 'incomplete') +
        ` · ${data.state.total_rounds ?? '?'} rounds`));
    }
    timeline.appendChild(head);

    for (const r of data.rounds) {
      newRound(r.round === 0 ? 'planning' : r.round, '');
      if (r.writer) renderProse(colFor(writerName), 'stream', r.writer);
      if (r.reviewer) renderProse(colFor(reviewerName), 'stream', r.reviewer);
      if (Array.isArray(r.checks)) {
        for (const c of r.checks) {
          line(currentRound.reviewerCol, c.passed ? 'check ok' : 'check bad',
            (c.passed ? '✓ ' : '✗ ') + c.name);
        }
      }
      if (r.clarification) fullWidth('info', 'user clarification: ' + r.clarification);
      if (r.patchPath) {
        const btn = el('button', 'link', 'open patch for round ' + r.round);
        btn.onclick = () => vscode.postMessage({ type: 'openFile', path: r.patchPath });
        const row = el('div', 'row');
        row.appendChild(btn);
        timeline.appendChild(row);
      }
    }
    currentRound = null;
    scrollDown();
  }

  // ── composer ───────────────────────────────────────────────

  /** Fill the project picker from the workspace folders the extension sent. */
  function setProjects(projects) {
    const chosen = projectSel.value;
    projectSel.innerHTML = '';
    for (const p of projects) {
      const opt = el('option', null, p.name);
      opt.value = p.path;
      projectSel.appendChild(opt);
    }
    projectSel.classList.toggle('hidden', projects.length < 2);
    // Rebuilding the options resets the picker to the first project. Restoring
    // the choice matters because the picker decides where a task writes: a
    // folder added mid-session must not silently redirect the next task.
    selectProject(chosen);
  }

  /** Select `path` if the picker knows it; ignored when it does not. */
  function selectProject(path) {
    for (const opt of projectSel.options) {
      if (opt.value === path) {
        projectSel.value = path;
        return;
      }
    }
  }

  function submitTask() {
    const text = input.value.trim();
    if (!text || busy) return;
    input.value = '';
    chips.innerHTML = '';
    vscode.postMessage({
      type: 'task',
      text,
      auto: autoBox.checked,
      plan: planBox.checked,
      dir: projectSel.value || undefined,
    });
  }

  document.getElementById('send').onclick = submitTask;
  document.getElementById('attach').onclick = () => vscode.postMessage({ type: 'attach' });
  document.getElementById('settings').onclick = () => vscode.postMessage({ type: 'settings' });
  document.getElementById('review').onclick = () => {
    if (!busy) {
      vscode.postMessage({ type: 'review', text: input.value.trim() });
      setBusy(true);
    }
  };

  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submitTask();
    }
  });

  // Real Cmd+V screenshot paste: images on the clipboard land here.
  input.addEventListener('paste', (e) => {
    for (const item of e.clipboardData.items) {
      if (item.type.startsWith('image/')) {
        e.preventDefault();
        const blob = item.getAsFile();
        const reader = new FileReader();
        reader.onload = () => {
          const b64 = String(reader.result).split(',')[1];
          vscode.postMessage({ type: 'pastedImage', dataB64: b64 });
        };
        reader.readAsDataURL(blob);
      }
    }
  });

  window.addEventListener('message', (e) => {
    const msg = e.data;
    switch (msg.type) {
      case 'event':
        onEvent(msg.ev);
        break;
      case 'history':
        renderHistory(msg.data);
        break;
      case 'attached':
        chips.appendChild(el('span', 'chip', '🖼 ' + msg.name));
        break;
      case 'projects':
        setProjects(msg.projects);
        break;
      case 'serveExit':
        fullWidth('error', `dt serve exited (code ${msg.code}) — next task restarts it`);
        setBusy(false);
        break;
    }
  });

  // The panel posts the project list as soon as it is constructed, which can be
  // before this script is listening. Asking once we are means the picker is
  // never left empty — and an empty picker means a task with no chosen project.
  vscode.postMessage({ type: 'ready' });
})();
