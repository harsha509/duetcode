const test = require('node:test');
const assert = require('node:assert/strict');
const { loadWebview, find, findAll } = require('./dom.js');

const PATHS = ['database/models.py', 'main.py', 'routers/webhooks/vapi_webhook_router.py'];
const REVIEW = `Reviewing agentic-testing-service (main) — 3 file(s): ${PATHS.join(', ')}

The webhook router looks sound.`;

/** The panel after `text` arrives as the reviewer's answer. */
function reviewed(text) {
  const w = loadWebview();
  w.send({ type: 'event', ev: { event: 'response', model: 'gemini', text } });
  return w.byId.timeline;
}

test('the reviewer opening line collapses to one sentence over the paths', () => {
  const card = find(reviewed(REVIEW), 'files-read');
  assert.ok(card, 'no files-read card rendered');

  const head = find(card, 'files-head');
  assert.equal(head.textContent, '▸Reviewing agentic-testing-service (main) — 3 files read');

  const list = find(card, 'files-list');
  assert.ok(list.classList.contains('hidden'), 'the paths start expanded');
  assert.deepEqual(findAll(list, 'files-item').map((i) => i.textContent), PATHS);
});

test('the toggle opens and closes the path list', () => {
  const card = find(reviewed(REVIEW), 'files-read');
  const head = find(card, 'files-head');
  const list = find(card, 'files-list');

  head.onclick();
  assert.equal(list.classList.contains('hidden'), false);
  assert.equal(find(card, 'files-caret').textContent, '▾');

  head.onclick();
  assert.equal(list.classList.contains('hidden'), true);
  assert.equal(find(card, 'files-caret').textContent, '▸');
});

test('a streamed review collapses the same way once it seals', () => {
  const w = loadWebview();
  const ev = (e) => w.send({ type: 'event', ev: e });
  ev({ event: 'stream_start', model: 'gemini' });
  ev({ event: 'stream_chunk', model: 'gemini', text: REVIEW.slice(0, 40) });
  ev({ event: 'stream_chunk', model: 'gemini', text: REVIEW.slice(40) });
  ev({ event: 'stream_end', model: 'gemini' });

  const card = find(w.byId.timeline, 'files-read');
  assert.ok(card, 'no files-read card rendered');
  assert.deepEqual(findAll(card, 'files-item').map((i) => i.textContent), PATHS);
});

test('the comma list leaves the prose, and the review itself stays', () => {
  const timeline = reviewed(REVIEW);
  const prose = find(timeline, 'block').textContent;
  assert.ok(!prose.includes('database/models.py'), 'the path list is still in the prose');
  assert.ok(prose.includes('The webhook router looks sound.'));
});

test('an answer that does not open with the line is left alone', () => {
  const plain = 'The webhook router looks sound.\n\nReviewing repo (main) — 2 file(s): a.py, b.py';
  const timeline = reviewed(plain);
  assert.equal(find(timeline, 'files-read'), undefined);
  assert.ok(find(timeline, 'block').textContent.includes('2 file(s): a.py, b.py'));
});

/** The panel after the given events, in order. */
function panel(events) {
  const w = loadWebview();
  for (const ev of events) w.send({ type: 'event', ev });
  return w.byId.timeline;
}

test('a seat is named once per round, not again after a full-width row splits it', () => {
  const timeline = panel([
    { event: 'ready', writer: 'claude', reviewer: 'gemini' },
    { event: 'working', actor: 'gemini', action: 'reviewing uncommitted changes…' },
    { event: 'info', text: 'no checks configured (.duet/config.toml [checks])' },
    { event: 'working', actor: 'gemini', action: 'reviewing uncommitted changes…' },
  ]);
  assert.deepEqual(findAll(timeline, 'col-head').map((h) => h.textContent), ['gemini · reviewer']);
});

test('a standalone review prints no round head, a real round still does', () => {
  const review = panel([{ event: 'working', actor: 'gemini', action: 'reviewing…' }]);
  assert.deepEqual(findAll(review, 'round-head'), []);

  const round = panel([
    { event: 'round_started', round: 1, budget: 3 },
    { event: 'working', actor: 'claude', action: 'writing…' },
  ]);
  assert.deepEqual(findAll(round, 'round-head').map((h) => h.textContent), ['round 1/3']);
});
