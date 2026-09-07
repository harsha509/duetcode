// Resolves the `vscode` module the compiled sources import to the stand-in in
// this directory, so they load under node's test runner instead of VS Code.
const Module = require('module');
const path = require('path');

const shim = path.join(__dirname, 'vscode.js');
const resolve = Module._resolveFilename;
Module._resolveFilename = function (request, ...rest) {
  return request === 'vscode' ? shim : resolve.call(this, request, ...rest);
};
