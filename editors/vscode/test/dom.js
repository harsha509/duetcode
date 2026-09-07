// A DOM small enough to run the webview script under node's test runner: the
// node, class-list, and event surface `media/main.js` actually touches, and
// nothing beyond it.
const fs = require('fs');
const path = require('path');
const vm = require('vm');

class Node {
  constructor(tag) {
    this.tag = String(tag);
    this.children = [];
    this.parent = null;
    this.classes = new Set();
    this.text = '';
    this.options = [];
    this.listeners = {};
  }

  get className() {
    return [...this.classes].join(' ');
  }

  set className(value) {
    this.classes = new Set(String(value).split(/\s+/).filter(Boolean));
  }

  get classList() {
    const node = this;
    return {
      add: (c) => node.classes.add(c),
      remove: (c) => node.classes.delete(c),
      contains: (c) => node.classes.has(c),
      toggle: (c, force) => {
        const on = force === undefined ? !node.classes.has(c) : force;
        if (on) node.classes.add(c);
        else node.classes.delete(c);
        return on;
      },
    };
  }

  get textContent() {
    return this.children.length ? this.children.map((c) => c.textContent).join('') : this.text;
  }

  set textContent(value) {
    this.children = [];
    this.text = String(value);
  }

  set innerHTML(value) {
    if (value) throw new Error('the shim only clears innerHTML');
    this.children = [];
    this.text = '';
  }

  appendChild(child) {
    // A fragment contributes its children, not itself — how the real one splices.
    for (const node of child.tag === '#fragment' ? child.children.splice(0) : [child]) {
      if (node.parent) node.parent.children.splice(node.parent.children.indexOf(node), 1);
      node.parent = this;
      this.children.push(node);
    }
    return child;
  }

  remove() {
    if (this.parent) this.parent.children.splice(this.parent.children.indexOf(this), 1);
    this.parent = null;
  }

  replaceWith(...nodes) {
    const at = this.parent.children.indexOf(this);
    for (const node of nodes) node.parent = this.parent;
    this.parent.children.splice(at, 1, ...nodes);
    this.parent = null;
  }

  querySelectorAll(selector) {
    const want = selector.split('.').filter(Boolean);
    return descendants(this).filter((n) => want.every((c) => n.classes.has(c)));
  }

  addEventListener(type, fn) {
    (this.listeners[type] ||= []).push(fn);
  }

  focus() {}
}

function descendants(node) {
  return node.children.flatMap((c) => [c, ...descendants(c)]);
}

/** Every descendant of `node` carrying `cls`, `node` itself included. */
function findAll(node, cls) {
  return [node, ...descendants(node)].filter((n) => n.classes.has(cls));
}

function find(node, cls) {
  return findAll(node, cls)[0];
}

/**
 * Loads `media/main.js` against the shim, returning its elements by id plus a
 * `send` that delivers one webview message the way the extension does.
 */
function loadWebview() {
  const byId = {};
  const document = {
    getElementById: (id) => (byId[id] ||= new Node('div')),
    createElement: (tag) => new Node(tag),
    createTextNode: (t) => Object.assign(new Node('#text'), { text: String(t) }),
    createDocumentFragment: () => new Node('#fragment'),
  };
  const window = new Node('#window');
  const posted = [];
  const sandbox = {
    document,
    window,
    console,
    acquireVsCodeApi: () => ({ postMessage: (m) => posted.push(m) }),
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '..', 'media', 'main.js'), 'utf8'),
    vm.createContext(sandbox));
  return {
    byId,
    posted,
    send: (msg) => {
      for (const fn of window.listeners.message || []) fn({ data: msg });
    },
  };
}

module.exports = { loadWebview, find, findAll };
