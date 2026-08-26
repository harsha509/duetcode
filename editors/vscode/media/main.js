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
  // What the column heads and the models line say. Separate from the two names
  // above, which route live events to a column: a replayed session is labelled
  // by what it recorded, while routing still follows the models in play.
  let labels = { writer: undefined, reviewer: undefined };
  let currentRound = null; // { writerCol, reviewerCol }
  let streams = {}; // model -> <pre> currently receiving chunks
  let activities = {}; // model -> the one live activity line, updated in place
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

  /** The models a live run is using: they route its events and name its columns. */
  function setLiveRoles(writer, reviewer) {
    writerName = writer;
    reviewerName = reviewer;
    labels = { writer, reviewer };
    modelsEl.textContent = `${writer} writes · ${reviewer} reviews`;
  }

  // Columns are created on first use, so a round only shows the blocks that
  // actually have content (no empty reviewer box while the writer streams).
  function colForSide(side) {
    if (!currentRound) newRound('•', '');
    // Recreated after a full-width row split the round, so later column
    // content lands below that row instead of above it.
    if (!currentRound.cols) {
      currentRound.cols = el('div', 'cols');
      currentRound.block.appendChild(currentRound.cols);
    }
    const key = side + 'Col';
    if (!currentRound[key]) {
      const col = el('div', 'col ' + side);
      // Bare role when no model is known: a session that recorded none is not
      // evidence that today's models ran it.
      const name = labels[side];
      col.appendChild(el('div', 'col-head', name ? name + ' · ' + side : side));
      currentRound.cols.appendChild(col);
      currentRound[key] = col;
    }
    return currentRound[key];
  }

  function colFor(actor) {
    return colForSide(sideFor(actor));
  }

  function line(target, cls, text) {
    target.appendChild(el('div', cls, text));
    scrollDown();
  }

  /** A row spanning the timeline. Mid-round it goes inside the round block,
      in arrival order — later column content restarts below it. */
  function fullWidthNode(node) {
    if (currentRound) {
      currentRound.block.appendChild(node);
      currentRound.cols = null;
      currentRound.writerCol = null;
      currentRound.reviewerCol = null;
    } else {
      timeline.appendChild(node);
    }
    scrollDown();
  }

  function fullWidth(cls, text) {
    fullWidthNode(el('div', 'row ' + cls, text));
  }

  /** The diffstat row, with the round's actual patch behind a toggle. The
      card is built once on first open and hidden on close, never rebuilt. */
  function changesRow(stat, diff) {
    const row = el('div', 'row changes', stat);
    if (diff) {
      const btn = el('button', 'link', 'view diff');
      let card = null;
      btn.onclick = () => {
        if (!card) {
          card = codeCard({ lang: 'diff', text: diff });
          row.appendChild(card);
          btn.textContent = 'hide diff';
          scrollDown();
        } else {
          const hidden = card.style.display === 'none';
          card.style.display = hidden ? '' : 'none';
          btn.textContent = hidden ? 'hide diff' : 'view diff';
        }
      };
      row.appendChild(document.createTextNode('\n'));
      row.appendChild(btn);
    }
    return row;
  }

  /** The model's current step — thinking, a tool call — as one pulsing line
      updated in place, instead of a stacked log of every action. */
  function showActivity(model, text) {
    const col = colFor(model);
    let a = activities[model];
    if (!a) {
      a = el('div', 'activity', '');
      activities[model] = a;
    }
    a.textContent = text;
    col.appendChild(a); // creates or moves it to the end, following the flow
    scrollDown();
  }

  /** Removes a model's activity line — its output speaks for itself now. */
  function settleActivity(model) {
    const a = activities[model];
    if (a) a.remove();
    delete activities[model];
  }

  function settleAllActivities() {
    for (const model of Object.keys(activities)) settleActivity(model);
  }

  function newRound(label, budget) {
    settleAllActivities();
    const block = el('section', 'round');
    const head = el('div', 'round-head', budget ? `round ${label}/${budget}` : String(label));
    block.appendChild(head);
    timeline.appendChild(block);
    currentRound = { block, cols: null, writerCol: null, reviewerCol: null };
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

  /**
   * Drops prose tinted as a blocker down to a warning, across `root`.
   *
   * The keyword tint is a guess made line by line, before any verdict exists;
   * the blocker list is what the reviewer actually concluded. When it is empty,
   * blocker-red prose contradicts the verdict printed directly beneath it —
   * which is the whole of what made a passing review read like a failing one.
   * Yellow, not cleared: the line still says something worth reading.
   */
  function capSeverity(root) {
    for (const span of root.querySelectorAll('.pl.issue')) {
      span.className = 'pl warn';
    }
  }

  function renderVerdict(target, kind, approved, blockers, suggestions) {
    // The round, not just this column: the verdict speaks for everything the
    // round produced, and half a round left red reads as a disagreement.
    //
    // Only on an approval. A rejected round with no parsed blockers — which is
    // what an unparseable verdict comes back as — has the tinted prose as its
    // only signal of what went wrong, and softening that would hide it.
    if (approved && !(blockers || []).length) {
      capSeverity(currentRound ? currentRound.block : target);
    }
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

  /** Inline `code` within one segment, so a symbol is not read as a word. */
  function appendCoded(parent, text) {
    let last = 0;
    const re = /`([^`\n]+)`/g;
    let m;
    while ((m = re.exec(text)) !== null) {
      if (m.index > last) parent.appendChild(document.createTextNode(text.slice(last, m.index)));
      parent.appendChild(el('code', 'inline', m[1]));
      last = re.lastIndex;
    }
    if (last < text.length) parent.appendChild(document.createTextNode(text.slice(last)));
  }

  /** Inline `code` and **bold** inside a prose line; code works inside bold. */
  function fillInline(span, raw) {
    let last = 0;
    const re = /\*\*([^*\n]+)\*\*/g;
    let m;
    while ((m = re.exec(raw)) !== null) {
      if (m.index > last) appendCoded(span, raw.slice(last, m.index));
      const bold = el('strong', 'bold');
      appendCoded(bold, m[1]);
      span.appendChild(bold);
      last = re.lastIndex;
    }
    appendCoded(span, raw.slice(last));
    span.appendChild(document.createTextNode('\n'));
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
        // A finding is something to act on, so it never goes green. "Verified"
        // and "Sound" inside one describe the checking the reviewer did, not an
        // all-clear, and green states the one thing the line does not say.
        const own = severityOf(raw);
        severity = (own === 'ok' ? '' : own) || inherited();
      }

      // Severity and section state are read off the raw line above; only the
      // display form drops the markdown scaffolding.
      let display = raw;
      if (level > 0) {
        display = raw.replace(/^\s*#{1,6}\s*/, '');
      } else if (/^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(raw)) {
        pre.appendChild(document.createTextNode('\n')); // a rule is just a pause
        continue;
      } else {
        display = raw.replace(/^(\s*)[*-]\s+/, '$1• ');
      }

      const cls = 'pl' + (severity ? ' ' + severity : '') + (level > 0 ? ' head' : '');
      const span = el('span', cls);
      fillInline(span, display);
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

  // ── PR verdict block ───────────────────────────────────────

  // Asked for by the prompt when a task names pull requests. Prefixed lines
  // rather than a fence, so the same text reads plainly in the terminal.
  const VERDICT_HEAD = /^VERDICT:\s*(GO|NO-GO)\b/i;
  const VERDICT_FINDING = /^(BLOCKER|WARNING):\s*(.+)$/i;

  /**
   * Lifts the verdict block off the front of an answer.
   *
   * Only a *leading* run counts: the words appear in ordinary review prose too,
   * and a summary assembled from sentences scattered through the analysis would
   * be a different, worse review than the one the model actually wrote.
   * Returns null when the answer does not open with one, which is every task
   * that is not about a pull request.
   */
  function takeVerdict(text) {
    const lines = text.split('\n');
    let i = 0;
    while (i < lines.length && !lines[i].trim()) i++;
    const head = VERDICT_HEAD.exec((lines[i] || '').trim());
    if (!head) {
      return null;
    }
    const findings = [];
    for (i++; i < lines.length; i++) {
      const line = lines[i].trim();
      if (!line) continue;
      const found = VERDICT_FINDING.exec(line);
      if (!found) break;
      findings.push({ blocking: found[1].toUpperCase() === 'BLOCKER', text: found[2] });
    }
    return {
      go: head[1].toUpperCase() === 'GO',
      findings,
      rest: lines.slice(i).join('\n'),
    };
  }

  function countLabel(n, word) {
    return `${n} ${word}${n === 1 ? '' : 's'}`;
  }

  function verdictGroup(card, title, findings, cls) {
    if (!findings.length) {
      return;
    }
    card.appendChild(el('div', 'pr-verdict-group', title));
    for (const f of findings) {
      card.appendChild(el('div', 'pr-verdict-item ' + cls, f.text));
    }
  }

  function verdictCard(v) {
    const blockers = v.findings.filter((f) => f.blocking);
    const warnings = v.findings.filter((f) => !f.blocking);
    const card = el('div', 'pr-verdict ' + (v.go ? 'go' : 'nogo'));

    const head = el('div', 'pr-verdict-head');
    head.appendChild(el('span', 'pr-verdict-badge', v.go ? '✓ GO' : '✕ NO-GO'));
    head.appendChild(
      el(
        'span',
        'pr-verdict-counts',
        `${countLabel(blockers.length, 'blocker')} · ${countLabel(warnings.length, 'warning')}`,
      ),
    );
    card.appendChild(head);

    verdictGroup(card, 'Blockers', blockers, 'bad');
    verdictGroup(card, 'Warnings', warnings, 'warn');
    return card;
  }

  /**
   * An answer as nodes: its verdict card, when it opens with one, then the
   * prose. Shared so a streamed answer, a whole one, and a replayed session all
   * render the same.
   */
  function answerNodes(text) {
    const nodes = [];
    const v = takeVerdict(text);
    if (v) {
      nodes.push(verdictCard(v));
      text = v.rest;
    }
    const block = el('div', 'block');
    fillProse(block, text);
    nodes.push(block);
    return nodes;
  }

  function renderProse(target, text) {
    for (const node of answerNodes(text)) {
      target.appendChild(node);
    }
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
    pre.replaceWith(...answerNodes(pre.textContent));
  }

  // ── live events ────────────────────────────────────────────

  function onEvent(ev) {
    switch (ev.event) {
      case 'ready':
        setLiveRoles(ev.writer, ev.reviewer);
        break;
      case 'task_started': {
        // Also restores the labels after a past session was on screen.
        setLiveRoles(ev.writer, ev.reviewer);
        currentRound = null;
        const head = el('div', 'task-head');
        // Just the task, as a separator between runs — mode and round budget
        // are noise here; the round headers already carry the budget.
        head.appendChild(el('span', 'task-title', ev.task));
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
        // Announced only in a multi-project workspace; with one project the
        // line restates the obvious.
        selectProject(ev.path);
        if (projectSel.options.length > 1) {
          fullWidth('project', '📁 ' + ev.name + ' — ' + ev.path);
        }
        break;
      case 'working':
        line(colFor(ev.actor), 'working', '● ' + ev.actor + ' — ' + ev.action);
        break;
      case 'thinking':
        sealStream(ev.model);
        showActivity(ev.model, '◌ thinking…');
        break;
      case 'tool_action':
        sealStream(ev.model);
        showActivity(ev.model, '⚡ ' + ev.desc);
        break;
      case 'stream_start': {
        // A previous stream left open — by an error, or a round that ended
        // without its end event — would otherwise be stranded as raw <pre>.
        sealStream(ev.model);
        settleActivity(ev.model);
        const pre = el('pre', 'stream');
        colFor(ev.model).appendChild(pre);
        streams[ev.model] = pre;
        break;
      }
      case 'stream_chunk': {
        settleActivity(ev.model);
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
        settleActivity(ev.model);
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
        fullWidthNode(changesRow(ev.stat.trim(), ev.diff));
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
        // Terminal plumbing, not panel content: the sessions view covers the
        // logs path, and the extension itself sent the workspace list.
        if (/^(workspace|logs): /.test(ev.text)) break;
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
        settleAllActivities();
        const style = OUTCOME_STYLES[ev.outcome] ?? OUTCOME_STYLES.stopped;
        fullWidth(style.cls, `${style.label} — ${ev.message} (${ev.rounds} rounds)`);
        setBusy(false);
        break;
      }
      case 'error':
        settleAllActivities();
        fullWidth('error', '✗ ' + ev.message);
        setBusy(false);
        break;
    }
  }

  // ── history rendering ──────────────────────────────────────

  function renderHistory(data) {
    timeline.innerHTML = '';
    currentRound = null;
    // Labelled by what this session recorded, never by the current setting —
    // switching the writer must not rewrite who ran a finished session.
    const roles = data.roles || {};
    labels = { writer: roles.writer, reviewer: roles.reviewer };
    modelsEl.textContent = labels.writer
      ? `${labels.writer} wrote · ${labels.reviewer} reviewed`
      : 'past session — models not recorded';
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
      // By side, not by model name: a stored round's columns are fixed by which
      // file it came from, and owe nothing to who is configured now.
      if (r.writer) renderProse(colForSide('writer'), r.writer);
      if (r.reviewer) renderProse(colForSide('reviewer'), r.reviewer);
      if (Array.isArray(r.checks)) {
        for (const c of r.checks) {
          line(colForSide('reviewer'), c.passed ? 'check ok' : 'check bad',
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

    // The same subordination a live verdict applies, from the only verdict a
    // stored session keeps — and on the same terms: a recorded approval AND no
    // blockers. A session that ended in changes-requested keeps its tinted
    // prose, which in a replay is the only trace of what was objected to.
    //
    // The blocker list must actually be an empty array. A missing field is not
    // evidence of a clean run, and `success` is not the test either: it goes
    // false when the checks fail on a review that approved.
    const state = data.state || {};
    const wasApproved = String(state.final_verdict || '').toLowerCase() === 'approved';
    if (wasApproved && Array.isArray(state.blockers) && state.blockers.length === 0) {
      capSeverity(timeline);
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
