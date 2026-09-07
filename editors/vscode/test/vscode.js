// The slice of the VS Code API the tested sources touch, with no behaviour
// beyond what a test needs to observe.
class EventEmitter {
  constructor() {
    this.listeners = [];
    this.event = (listener) => {
      this.listeners.push(listener);
      return { dispose: () => this.listeners.splice(this.listeners.indexOf(listener), 1) };
    };
  }
  fire(value) {
    for (const listener of [...this.listeners]) listener(value);
  }
  dispose() {}
}

class Disposable {
  constructor(fn) {
    this.fn = fn;
  }
  dispose() {
    this.fn?.();
  }
}

class TreeItem {
  constructor(label, collapsibleState) {
    this.label = label;
    this.collapsibleState = collapsibleState;
  }
}

class ThemeIcon {
  constructor(id) {
    this.id = id;
  }
}

module.exports = {
  EventEmitter,
  Disposable,
  TreeItem,
  ThemeIcon,
  TreeItemCollapsibleState: { None: 0, Collapsed: 1, Expanded: 2 },
  workspace: { workspaceFolders: undefined },
  window: { showErrorMessage: async () => undefined, showInformationMessage: async () => undefined },
  env: { clipboard: { writeText: async () => {} } },
  commands: { executeCommand: async () => {} },
};
