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

  // How each run outcome closes the timeline. 'unreviewed' is deliberately
  // neutral: the user declined the review, which is neither pass nor fail.
  const OUTCOME_STYLES = {
    approved: { cls: 'success', label: 'SUCCESS' },
    unreviewed: { cls: 'neutral', label: 'NO REVIEW' },
    stopped: { cls: 'warn', label: 'STOPPED' },
  };

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

  // An answer review judges an answer, not the code that answer is about — and
  // those are regularly opposite, since an answer can soundly argue against a
  // change. Rendering both as APPROVED said the reviewer endorsed the change.
  // `kind` is absent on verdicts from an older CLI, which only ever sent code.
  function verdictLabel(kind, approved) {
    if (kind === 'answer') { return approved ? 'SOUND' : 'UNSOUND'; }
    return approved ? 'APPROVED' : 'CHANGES REQUESTED';
  }

  function renderVerdict(target, kind, approved, blockers, suggestions) {
    const chip = el('div', 'verdict ' + (approved ? 'ok' : 'bad'),
      verdictLabel(kind, approved));
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

  // ── code blocks ────────────────────────────────────────────

  // Prose and code arrive as one monospace wall, which reads as neither. A
  // fenced block becomes its own card — labelled, tinted, scrolling rather than
  // wrapping — so what is being *shown* is told apart from what is being said.

  const FENCE = /^\s{0,3}(`{3,}|~{3,})[ \t]*([\w+#.-]*)[ \t]*$/;

  /** Model text as an ordered run of prose and fenced code parts. */
  function splitFences(text) {
    const parts = [];
    let prose = [];
    let code = null;

    const flushProse = () => {
      if (prose.length) {
        parts.push({ code: false, text: prose.join('\n') });
        prose = [];
      }
    };

    for (const raw of text.split('\n')) {
      const fence = FENCE.exec(raw);
      if (code) {
        // Only the same fence character closes a block, so a ``` inside a ~~~
        // block stays content — which is how a model quotes markdown at us.
        if (fence && fence[1][0] === code.mark[0] && fence[1].length >= code.mark.length) {
          parts.push({ code: true, lang: code.lang, text: code.lines.join('\n') });
          code = null;
        } else {
          code.lines.push(raw);
        }
        continue;
      }
      if (fence) {
        flushProse();
        code = { mark: fence[1], lang: fence[2].toLowerCase(), lines: [] };
        continue;
      }
      prose.push(raw);
    }

    // An unclosed fence is ordinary in a cut-off or still-streaming answer:
    // render what arrived rather than dropping it.
    if (code) parts.push({ code: true, lang: code.lang, text: code.lines.join('\n') });
    flushProse();
    return parts;
  }

  // Two comment styles cover nearly everything a model writes here, so each
  // language maps to a family rather than carrying its own grammar. Highlighting
  // is meant to make structure visible, not to be a compiler.
  const FAMILY = {
    js: 'c', jsx: 'c', ts: 'c', tsx: 'c', javascript: 'c', typescript: 'c', java: 'c',
    c: 'c', h: 'c', cpp: 'c', 'c++': 'c', cc: 'c', cs: 'c', csharp: 'c', go: 'c', golang: 'c',
    rust: 'c', rs: 'c', swift: 'c', kotlin: 'c', kt: 'c', scala: 'c', php: 'c', dart: 'c',
    css: 'c', scss: 'c', proto: 'c',
    py: 'hash', python: 'hash', sh: 'hash', bash: 'hash', zsh: 'hash', shell: 'hash',
    console: 'hash', rb: 'hash', ruby: 'hash', yaml: 'hash', yml: 'hash', toml: 'hash',
    ini: 'hash', conf: 'hash', dockerfile: 'hash', makefile: 'hash', make: 'hash', perl: 'hash',
    json: 'json', json5: 'json',
    diff: 'diff', patch: 'diff',
    text: 'plain', txt: 'plain', log: 'plain', plain: 'plain', output: 'plain',
  };

  const words = (s) => new Set(s.split(' '));

  const SPECS = {
    c: {
      line: '//',
      block: true,
      kw: words('as async await break case catch class const continue default defer delete do' +
        ' else enum export extends extern false final finally fn for func function go if impl' +
        ' implements import in instanceof interface let loop match mod move mut new null nullptr' +
        ' override package private protected pub public return self static struct super switch' +
        ' this throw throws trait true try type typeof union unsafe use using var virtual void' +
        ' where while yield'),
    },
    hash: {
      line: '#',
      block: false,
      kw: words('and as assert async await break case class continue def del do done elif else' +
        ' esac except export False fi finally for from global if import in is lambda local nil' +
        ' None not or pass raise readonly return self source then True try unless until unset' +
        ' when while with yield'),
    },
    json: { line: null, block: false, kw: words('true false null') },
    plain: { plain: true },
  };

  /** One scanner per family, built on first use and reused across blocks. */
  function scannerFor(spec) {
    if (!spec.re) {
      const alts = [];
      if (spec.block) alts.push('/\\*[\\s\\S]*?(?:\\*/|$)');
      if (spec.line) alts.push(spec.line.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&') + '[^\\n]*');
      alts.push('"(?:\\\\.|[^"\\\\\\n])*"?');
      alts.push("'(?:\\\\.|[^'\\\\\\n])*'?");
      alts.push('`(?:\\\\.|[^`\\\\])*`?');
      alts.push('\\b\\d[\\w.]*');
      alts.push('[A-Za-z_$][\\w$]*');
      spec.re = new RegExp(alts.join('|'), 'g');
    }
    spec.re.lastIndex = 0;
    return spec.re;
  }

  function tokenClass(token, spec, code, end) {
    const head = token[0];
    if (head === '/' || (spec.line && token.startsWith(spec.line))) return 'com';
    if (head === '"' || head === "'" || head === '`') return 'str';
    if (head >= '0' && head <= '9') return 'num';
    if (spec.kw.has(token)) return 'kw';
    // A name followed by `(` is being called or defined; a capitalised one is a
    // type. Both are landmarks when skimming a block for what it does.
    if (/^\s*\(/.test(code.slice(end, end + 8))) return 'fn';
    if (head >= 'A' && head <= 'Z') return 'typ';
    return '';
  }

  /** Diffs are the one thing reviewed here, so their lines carry the colour. */
  function highlightDiff(code, frag) {
    for (const raw of code.split('\n')) {
      let cls = '';
      if (/^(\+\+\+|---|diff |index |new file|deleted file|similarity|rename )/.test(raw)) {
        cls = 'meta';
      } else if (raw.startsWith('@@')) cls = 'hunk';
      else if (raw.startsWith('+')) cls = 'add';
      else if (raw.startsWith('-')) cls = 'del';
      frag.appendChild(el('span', cls ? 'dl ' + cls : 'dl', raw + '\n'));
    }
  }

  function highlight(code, lang) {
    const frag = document.createDocumentFragment();
    const family = FAMILY[lang] || 'c';
    if (family === 'diff') {
      highlightDiff(code, frag);
      return frag;
    }

    const spec = SPECS[family];
    if (spec.plain) {
      frag.appendChild(document.createTextNode(code));
      return frag;
    }

    const re = scannerFor(spec);
    let last = 0;
    let m;
    while ((m = re.exec(code)) !== null) {
      if (m.index > last) frag.appendChild(document.createTextNode(code.slice(last, m.index)));
      const cls = tokenClass(m[0], spec, code, re.lastIndex);
      frag.appendChild(cls ? el('span', 'tok ' + cls, m[0]) : document.createTextNode(m[0]));
      last = re.lastIndex;
      // A zero-width match would spin forever; nothing in the scanner can
      // produce one, but the loop must not depend on that.
      if (re.lastIndex === m.index) re.lastIndex++;
    }
    if (last < code.length) frag.appendChild(document.createTextNode(code.slice(last)));
    return frag;
  }

  function codeCard(part) {
    const card = el('div', 'code');
    if (part.lang) card.appendChild(el('div', 'code-lang', part.lang));
    const body = el('pre', 'code-body');
    body.appendChild(highlight(part.text, part.lang));
    card.appendChild(body);
    return card;
  }

  /** Inline `code` inside a prose line, so a symbol is not read as a word. */
  function fillInline(span, raw) {
    let last = 0;
    const re = /`([^`\n]+)`/g;
    let m;
    while ((m = re.exec(raw)) !== null) {
      if (m.index > last) span.appendChild(document.createTextNode(raw.slice(last, m.index)));
      span.appendChild(el('code', 'inline', m[1]));
      last = re.lastIndex;
    }
    span.appendChild(document.createTextNode(raw.slice(last) + '\n'));
  }

  /**
   * Paints one run of prose, one span per line, tinted by severity.
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
   *
   * `state` carries the enclosing section across the code blocks that split a
   * run, so a fenced example under a heading does not end its section.
   */
  function proseRun(text, state) {
    const pre = el('pre', 'prose');
    const inherited = () =>
      (state.section && state.section.severity !== 'ok' ? state.section.severity : '');

    for (const raw of text.split('\n')) {
      const level = headingLevel(raw);
      let severity = '';

      if (level > 0) {
        severity = severityOf(raw);
        if (severity) {
          state.section = { level, severity };
        } else if (state.section && level > state.section.level) {
          severity = inherited();
        } else {
          state.section = null;
        }
      } else if (isFinding(raw)) {
        severity = severityOf(raw) || inherited();
      }

      const span = el('span', severity ? 'pl ' + severity : 'pl');
      fillInline(span, raw);
      pre.appendChild(span);
    }
    return pre;
  }

  /** Repaints `block` as prose runs and code cards. */
  function fillProse(block, text) {
    block.textContent = '';
    const state = { section: null };
    for (const part of splitFences(text)) {
      block.appendChild(part.code ? codeCard(part) : proseRun(part.text, state));
    }
  }

  function renderProse(target, text) {
    const block = el('div', 'block');
    fillProse(block, text);
    target.appendChild(block);
    scrollDown();
  }

  /**
   * Closes the open stream for `model`, laying out the text that arrived.
   *
   * Chunks accumulate in one <pre> pinned where it was created, while a tool or
   * thinking line appends to the end of the column. A turn that interleaves the
   * two — answer, grep, more answer — therefore renders every tool below text
   * that came after it, which is how the whole run's tools end up in a clump at
   * the bottom. Sealing at the boundary lets the next chunk open a fresh <pre>
   * after the tool line, so the column reads in the order things happened.
   *
   * The layout waits for the seal because a chunk can split a line — or a fence
   * — anywhere, and classifying mid-stream would colour half a heading and card
   * half a code block.
   */
  function sealStream(model) {
    const pre = streams[model];
    if (!pre) {
      return;
    }
    delete streams[model];
    // A stream sealed before its first chunk has nothing to lay out, and an
    // empty block would still take a paragraph's worth of space.
    if (!pre.textContent) {
      pre.remove();
      return;
    }
    const block = el('div', 'block');
    fillProse(block, pre.textContent);
    pre.replaceWith(block);
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
        sealStream(ev.model);
        line(colFor(ev.model), 'dim', '◌ thinking…');
        break;
      case 'tool_action':
        sealStream(ev.model);
        line(colFor(ev.model), 'tool', '⚡ ' + ev.desc);
        break;
      case 'stream_start': {
        // A previous stream left open — by an error, or a round that ended
        // without its end event — would otherwise be stranded as raw <pre>.
        sealStream(ev.model);
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
      case 'stream_end':
        sealStream(ev.model);
        break;
      case 'response':
        renderProse(colFor(ev.model), ev.text);
        break;
      case 'check':
        line(colFor(reviewerName), ev.passed ? 'check ok' : 'check bad',
          (ev.passed ? '✓ ' : '✗ ') + ev.name);
        break;
      case 'verdict':
        renderVerdict(colFor(reviewerName), ev.kind, ev.approved, ev.blockers, ev.suggestions);
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
      case 'task_done': {
        const style = OUTCOME_STYLES[ev.outcome] ?? OUTCOME_STYLES.stopped;
        fullWidth(style.cls, `${style.label} — ${ev.message} (${ev.rounds} rounds)`);
        setBusy(false);
        break;
      }
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
      if (r.writer) renderProse(colFor(writerName), r.writer);
      if (r.reviewer) renderProse(colFor(reviewerName), r.reviewer);
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
