import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { test } from "node:test";

function moduleUrl(source) {
  return `data:text/javascript,${encodeURIComponent(source)}`;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function element() {
  return {
    appendChild() {},
    remove() {},
    querySelector() {
      return null;
    },
    addEventListener() {},
    removeEventListener() {},
    className: "",
    clientHeight: 300,
    clientWidth: 800,
    isConnected: true,
  };
}

class MockTerminal {
  constructor(options) {
    this.options = options;
    this.cols = 80;
    this.rows = 24;
    this.textarea = element();
  }

  loadAddon() {}
  open() {}
  reset() {}
  write() {}
  writeln() {}
  focus() {}
  dispose() {}
  hasSelection() {
    return false;
  }
  getSelection() {
    return "";
  }
  onSelectionChange() {
    return { dispose() {} };
  }
  onScroll() {
    return { dispose() {} };
  }
  onData(callback) {
    this.onInput = callback;
    return { dispose() {} };
  }
  input(data) {
    this.onInput?.(data);
  }
  onResize() {
    return { dispose() {} };
  }
  onTitleChange() {
    return { dispose() {} };
  }
}

class MockFitAddon {
  fit() {}
}

class MockWebSocket {
  static OPEN = 1;

  constructor(url) {
    this.url = url;
    this.readyState = MockWebSocket.OPEN;
  }

  send() {}
  close() {
    this.readyState = 3;
  }
}

const mocks = {
  wb: {
    termClose: async () => {},
    termRelease: () => {},
    termResize: async () => {},
    termWrite: async () => {},
  },
  worldApi: {
    closeTerminal: async () => {},
    openTerminal: async () => ({ session_id: "unused", socket: "ws://unused" }),
  },
  typeCommand: async (_term, send, command, at) => {
    if (!at.alive()) return "cancelled";
    send(command);
    return "typed";
  },
};

globalThis.__terminalShellMocks = mocks;
globalThis.document = {
  body: element(),
  createElement: element,
};
globalThis.ResizeObserver = class {
  observe() {}
  disconnect() {}
};
globalThis.WebSocket = MockWebSocket;

const mockedModules = new Map([
  ["@xterm/xterm", moduleUrl(`export const Terminal = globalThis.__terminalShellMocks.Terminal;`)],
  ["@xterm/addon-fit", moduleUrl(`export const FitAddon = globalThis.__terminalShellMocks.FitAddon;`)],
  ["@/lib/terminal-keys", moduleUrl(`export function bindTerminalKeys() {}`)],
  ["@/lib/world", moduleUrl(`export const worldApi = globalThis.__terminalShellMocks.worldApi;`)],
  ["./workbench/bridge", moduleUrl(`export const wb = globalThis.__terminalShellMocks.wb;`)],
  [
    "./terminal-surface",
    moduleUrl(`
      export const TERMINAL_FONTS = "monospace";
      export const loadTerminalFont = async () => "monospace";
      export const readTerminal = () => null;
      export const reducedMotion = () => false;
      export const selectionSpot = () => null;
      export const terminalFontSize = () => 13;
      export const terminalTheme = () => ({});
      export const typeCommand = (...args) => globalThis.__terminalShellMocks.typeCommand(...args);
    `),
  ],
]);

mocks.Terminal = MockTerminal;
mocks.FitAddon = MockFitAddon;

const hooks = registerHooks({
  resolve(specifier, context, nextResolve) {
    const mocked = mockedModules.get(specifier);
    if (mocked) return { url: mocked, shortCircuit: true };
    if (specifier === "@/lib/terminal-close") return { url: new URL("./terminal-close.ts", import.meta.url).href, shortCircuit: true };
    if (specifier === "@/lib/terminal-lifecycle") return { url: new URL("./terminal-lifecycle.ts", import.meta.url).href, shortCircuit: true };
    return nextResolve(specifier, context);
  },
});

const { Shell } = await import("../components/world/terminal-shell.ts");
hooks.deregister();

function shell(id) {
  const native = [];
  const instance = new Shell(
    id,
    {
      changed() {},
      shortcut() {},
      focused() {},
      selection() {},
      native: (_pane, value) => native.push(value),
    },
    "web",
  );
  return { instance, native };
}

function resetMocks() {
  mocks.wb.termClose = async () => {};
  mocks.wb.termRelease = () => {};
  mocks.wb.termResize = async () => {};
  mocks.wb.termWrite = async () => {};
  mocks.worldApi.closeTerminal = async () => {};
  mocks.worldApi.openTerminal = async () => ({ session_id: "unused", socket: "ws://unused" });
  mocks.typeCommand = async (_term, send, command, at) => {
    if (!at.alive()) return "cancelled";
    send(command);
    return "typed";
  };
}

test("a fresh native open adopts the returned shell id", async () => {
  resetMocks();
  const { instance, native } = shell("fresh-pane");
  await instance.openLocal(async () => ({ id: "fresh-native", exited: false }), null, "this Mac");
  assert.equal(instance.native, "fresh-native");
  assert.equal(instance.phase, "live");
  assert.deepEqual(native, ["fresh-native"]);
  instance.dispose();
});

test("closing a pending native open cancels queued and later input", async () => {
  resetMocks();
  const opened = deferred();
  const closing = deferred();
  const writes = [];
  mocks.wb.termClose = () => closing.promise;
  mocks.wb.termWrite = async (_id, data) => writes.push(data);
  const { instance } = shell("pending-close-pane");
  const opening = instance.openLocal(() => opened.promise, null, "this Mac");
  const queued = instance.type("queued input");
  const close = instance.close();
  opened.resolve({ id: "pending-close-native", exited: false });
  await opening;
  assert.equal(await queued, null);
  assert.equal(await instance.type("later input"), null);
  await Promise.resolve();
  assert.deepEqual(writes, []);
  closing.resolve();
  assert.equal((await close).status, "closed");
  instance.dispose();
});

test("normal terminal input stops during close and resumes after a failed close", async () => {
  resetMocks();
  const closing = deferred();
  const writes = [];
  mocks.wb.termClose = () => closing.promise;
  mocks.wb.termWrite = async (_id, data) => writes.push(data);
  const { instance } = shell("normal-input-close-pane");
  await instance.openLocal(async () => ({ id: "normal-input-close-native", exited: false }), null, "this Mac");
  instance.term.input("before close");
  const close = instance.close();
  instance.term.input("during close");
  closing.reject(new Error("offline"));
  assert.equal((await close).status, "failed");
  instance.term.input("after failed close");
  assert.deepEqual(writes, ["before close", "after failed close"]);
  instance.dispose();
});

test("a failed close still cancels input that was waiting to type", async () => {
  resetMocks();
  const ready = deferred();
  const writes = [];
  mocks.typeCommand = async (_term, send, command, at) => {
    await ready.promise;
    if (!at.alive()) return "cancelled";
    send(command);
    return "typed";
  };
  mocks.wb.termClose = async () => {
    throw new Error("offline");
  };
  mocks.wb.termWrite = async (_id, data) => writes.push(data);
  const { instance } = shell("failed-close-input-pane");
  await instance.openLocal(async () => ({ id: "failed-close-input-native", exited: false }), null, "this Mac");
  const typed = instance.type("waiting input");
  assert.equal((await instance.close()).status, "failed");
  ready.resolve();
  assert.equal(await typed, "cancelled");
  assert.deepEqual(writes, []);
  instance.dispose();
});

test("a failed native close preserves its id and retries the same shell", async () => {
  resetMocks();
  const closes = [];
  let attempts = 0;
  mocks.wb.termClose = async (id) => {
    closes.push(id);
    attempts += 1;
    if (attempts === 1) throw new Error("offline");
  };
  const { instance, native } = shell("native-retry-pane");
  await instance.openLocal(async () => ({ id: "retry-native", exited: false }), null, "this Mac");
  const failed = await instance.close();
  assert.equal(failed.status, "failed");
  assert.equal(instance.native, "retry-native");
  assert.equal(instance.phase, "live");
  assert.deepEqual(native, ["retry-native"]);
  const closed = await instance.close();
  assert.equal(closed.status, "closed");
  assert.equal(instance.native, null);
  assert.equal(instance.phase, "idle");
  assert.deepEqual(closes, ["retry-native", "retry-native"]);
  assert.deepEqual(native, ["retry-native", null]);
  instance.dispose();
});

test("an empty remote close error preserves the exact session for retry", async () => {
  resetMocks();
  const closes = [];
  let attempts = 0;
  mocks.worldApi.openTerminal = async () => ({ session_id: "retry-remote", socket: "ws://retry-remote" });
  mocks.worldApi.closeTerminal = async (id) => {
    closes.push(id);
    attempts += 1;
    if (attempts === 1) throw new Error("");
  };
  const { instance } = shell("remote-retry-pane");
  await instance.openRemote("Build Mac");
  const failed = await instance.close();
  assert.deepEqual(failed, { status: "failed", error: "could not close the remote shell" });
  assert.equal(instance.phase, "starting");
  const closed = await instance.close();
  assert.equal(closed.status, "closed");
  assert.equal(instance.phase, "idle");
  assert.deepEqual(closes, ["retry-remote", "retry-remote"]);
  instance.dispose();
});

test("same-id attaches serialize and late contenders never kill a canvas native id", async () => {
  resetMocks();
  const closes = [];
  mocks.wb.termClose = async (id) => closes.push(id);
  const canvasOpen = deferred();
  const canvas = shell("canvas-pane").instance;
  const panel = shell("panel-pane").instance;
  const canvasOpening = canvas.openLocal(() => canvasOpen.promise, "shared-native", "this Mac");
  await Promise.resolve();
  let panelOpens = 0;
  const panelOpening = panel.openLocal(async () => {
    panelOpens += 1;
    return { id: "shared-native", exited: false };
  }, "shared-native", "this Mac");
  await Promise.resolve();
  assert.equal(panelOpens, 0);
  const panelClosing = panel.close("shared-native");
  assert.equal((await panelClosing).status, "protected");
  canvasOpen.resolve({ id: "shared-native", exited: false });
  await canvasOpening;
  await panelOpening;
  assert.equal(panelOpens, 0);
  assert.equal(canvas.native, "shared-native");
  assert.equal(canvas.phase, "live");
  assert.equal(panel.native, null);
  assert.equal(panel.phase, "idle");
  const staleOpen = deferred();
  const stale = shell("stale-pane").instance;
  const staleOpening = stale.openLocal(() => staleOpen.promise, "missing-native", "this Mac");
  await Promise.resolve();
  stale.dispose();
  staleOpen.resolve({ id: "shared-native", exited: false });
  await staleOpening;
  await Promise.resolve();
  assert.deepEqual(closes, []);
  panel.dispose();
  canvas.dispose();
});
