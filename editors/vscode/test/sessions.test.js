require('./register');
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const vscode = require('vscode');
const { SessionsProvider } = require('../out/sessions.js');

/** A workspace of the named folders, each holding one finished session. */
function workspace(names) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'dt-sessions-'));
  vscode.workspace.workspaceFolders = names.map((name, index) => {
    const dir = path.join(root, name);
    const session = path.join(dir, '.duet', 'sessions', '20260901-101010-old-task');
    fs.mkdirSync(session, { recursive: true });
    fs.writeFileSync(path.join(session, 'state.json'), JSON.stringify({ task: 'old task', success: true }));
    return { uri: { fsPath: dir }, name, index };
  });
  return vscode.workspace.workspaceFolders.map((folder) => folder.uri.fsPath);
}

test('the running task is listed first, marked, and stoppable', () => {
  const [api] = workspace(['api']);
  const provider = new SessionsProvider(() => ({ label: 'add retries\nto uploads', project: api, since: Date.now() }));
  const [first, second] = provider.getChildren();
  assert.equal(first.kind, 'running');
  assert.equal(second.kind, 'session');

  const item = provider.getTreeItem(first);
  assert.equal(item.label, 'add retries');
  assert.equal(item.description, 'running…');
  assert.equal(item.contextValue, 'dtRunning');
  assert.equal(item.iconPath.id, 'loading~spin');
  assert.equal(item.command.command, 'dt.showPanel');
});

test('a running review lands under its own project in a multi-root workspace', () => {
  const [, web] = workspace(['api', 'web']);
  const provider = new SessionsProvider(() => ({ label: 'review', project: web, since: Date.now() }));
  const projects = provider.getChildren();
  assert.deepEqual(projects.map((p) => p.kind), ['project', 'project']);
  const [api, running] = projects;
  assert.equal(provider.getChildren(api)[0].kind, 'session');
  assert.deepEqual(provider.getChildren(running).map((n) => n.kind), ['running', 'session']);
});

/** A session directory stamped at `date` with the given slug and no summary yet. */
function unfinishedSession(root, date, slug) {
  const pad = (n) => String(n).padStart(2, '0');
  const stamp =
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-` +
    `${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
  fs.mkdirSync(path.join(root, '.duet', 'sessions', `${stamp}-${slug}`), { recursive: true });
}

test("the running task's own log is not listed a second time beneath it", () => {
  const [api] = workspace(['api']);
  const since = Date.now() - 500;
  unfinishedSession(api, new Date(since + 200), 'fresh-task');
  unfinishedSession(api, new Date(since - 3_600_000), 'stale-task');
  const provider = new SessionsProvider(() => ({ label: 'fresh task', project: api, since }));
  const rows = provider.getChildren();
  assert.deepEqual(rows.map((n) => n.kind), ['running', 'session', 'session']);
  const slugs = rows.slice(1).map((n) => n.name.slice(16));
  assert.deepEqual(slugs.sort(), ['old-task', 'stale-task']);
});

test('nothing is listed as running while the server is idle', () => {
  workspace(['api']);
  const provider = new SessionsProvider(() => undefined);
  assert.deepEqual(provider.getChildren().map((n) => n.kind), ['session']);
});
