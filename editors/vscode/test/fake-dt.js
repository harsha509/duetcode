#!/usr/bin/env node
// Stands in for `dt serve`: replays the event stream a task or a standalone
// review produces, and answers stop, ping, and quit the way the server does.
const readline = require('readline');

const emit = (event) => process.stdout.write(JSON.stringify(event) + '\n');

emit({ event: 'ready', writer: 'gemini', reviewer: 'claude', version: 'test' });

readline.createInterface({ input: process.stdin }).on('line', (line) => {
  const cmd = JSON.parse(line);
  switch (cmd.cmd) {
    case 'review':
      emit({ event: 'project_started', name: 'api', path: '/w/api' });
      emit({ event: 'working', actor: 'claude', action: 'reviewing uncommitted changes…' });
      break;
    case 'task':
      emit({ event: 'project_started', name: 'api', path: '/w/api' });
      emit({ event: 'task_started', task: cmd.task, writer: 'gemini', reviewer: 'claude', mode: 'auto', max_rounds: 4 });
      emit({ event: 'working', actor: 'gemini', action: 'writing…' });
      break;
    case 'stop':
      emit({ event: 'info', text: 'stopping…' });
      emit({ event: 'task_done', outcome: 'stopped', rounds: 1, message: 'stopped by user' });
      break;
    case 'ping':
      emit({ event: 'pong' });
      break;
    case 'quit':
      emit({ event: 'bye' });
      process.exit(0);
  }
});
