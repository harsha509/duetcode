require('./register');
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { ServeClient } = require('../out/serveClient.js');

const FAKE_DT = path.join(__dirname, 'fake-dt.js');

function client() {
  return new ServeClient(
    () => ({ binPath: FAKE_DT, writer: 'gemini', claudeModel: '', geminiModel: '' }),
    () => __dirname,
  );
}

/** Resolves with the next event of that name. */
function next(c, name) {
  return new Promise((resolve) => {
    const sub = c.onEvent((ev) => {
      if (ev.event === name) {
        sub.dispose();
        resolve(ev);
      }
    });
  });
}

test('a standalone review is running from project_started until it reports', async () => {
  const c = client();
  const working = next(c, 'working');
  c.send({ cmd: 'review' });
  await working;
  assert.equal(c.isRunning, true);
  assert.equal(c.currentActivity.label, 'review');
  assert.equal(c.currentActivity.project, '/w/api');
  assert.ok(Math.abs(c.currentActivity.since - Date.now()) < 5000);

  const done = next(c, 'task_done');
  c.stop();
  const outcome = await done;
  assert.equal(outcome.outcome, 'stopped');
  assert.equal(c.isRunning, false);
  assert.equal(c.currentActivity, undefined);

  // Stopping asked the server rather than killing it: it still answers.
  const pong = next(c, 'pong');
  c.send({ cmd: 'ping' });
  await pong;
  c.dispose();
});

test('a task is named by task_started and cleared by a restart', async () => {
  const c = client();
  const changes = [];
  c.onActivityChanged((activity) => changes.push(activity));
  const working = next(c, 'working');
  c.send({ cmd: 'task', task: 'add retries to the upload path' });
  await working;
  assert.equal(c.currentActivity.label, 'add retries to the upload path');
  assert.equal(c.currentActivity.project, '/w/api');
  assert.deepEqual(changes.map((a) => a.label), ['review', 'add retries to the upload path']);
  assert.equal(changes[0].since, changes[1].since);

  c.restart();
  assert.equal(c.isRunning, false);
  assert.equal(c.currentActivity, undefined);
});
