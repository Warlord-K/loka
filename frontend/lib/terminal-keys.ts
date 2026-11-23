import type { Terminal } from "@xterm/xterm";

export type TerminalAction =
  | "new_tab"
  | "split_right"
  | "split_down"
  | "close"
  | "focus_prev"
  | "focus_next"
  | "tab_prev"
  | "tab_next"
  | "focus_left"
  | "focus_right"
  | "focus_up"
  | "focus_down";

export type KeyLike = Pick<KeyboardEvent, "key" | "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey">;

export type KeyProfile = "app" | "web";

const ARROWS: Record<string, TerminalAction> = {
  ArrowLeft: "focus_left",
  ArrowRight: "focus_right",
  ArrowUp: "focus_up",
  ArrowDown: "focus_down",
};

const WEB_CODES: Record<string, TerminalAction> = {
  Enter: "new_tab",
  NumpadEnter: "new_tab",
  KeyX: "close",
  KeyD: "split_right",
  KeyE: "split_down",
  BracketLeft: "focus_prev",
  BracketRight: "focus_next",
  Comma: "tab_prev",
  Period: "tab_next",
};

const WEB_KEYS: Record<string, TerminalAction> = {
  Enter: "new_tab",
  x: "close",
  d: "split_right",
  e: "split_down",
  "[": "focus_prev",
  "{": "focus_prev",
  "]": "focus_next",
  "}": "focus_next",
  ",": "tab_prev",
  "<": "tab_prev",
  ".": "tab_next",
  ">": "tab_next",
};

export function keyProfile(inMacApp: boolean): KeyProfile {
  return inMacApp ? "app" : "web";
}

function bracketOf(e: KeyLike): "[" | "]" | null {
  if (e.code === "BracketLeft" || e.key === "[" || e.key === "{") return "[";
  if (e.code === "BracketRight" || e.key === "]" || e.key === "}") return "]";
  return null;
}

function appShortcut(e: KeyLike): TerminalAction | null {
  if (!e.metaKey || e.ctrlKey) return null;
  if (e.altKey) return e.shiftKey ? null : ARROWS[e.key] ?? null;
  const bracket = bracketOf(e);
  if (bracket) return e.shiftKey ? (bracket === "[" ? "tab_prev" : "tab_next") : bracket === "[" ? "focus_prev" : "focus_next";
  const key = e.key.toLowerCase();
  if (key === "d") return e.shiftKey ? "split_down" : "split_right";
  if (e.shiftKey) return null;
  return key === "t" ? "new_tab" : key === "w" ? "close" : null;
}

function webShortcut(e: KeyLike): TerminalAction | null {
  if (!e.ctrlKey || !e.shiftKey || e.metaKey || e.altKey) return null;
  return ARROWS[e.key] ?? WEB_CODES[e.code] ?? WEB_KEYS[e.key.length === 1 ? e.key.toLowerCase() : e.key] ?? null;
}

export function terminalShortcut(e: KeyLike, profile: KeyProfile): TerminalAction | null {
  return profile === "app" ? appShortcut(e) : webShortcut(e);
}

export type LabelledAction = "new_tab" | "split_right" | "split_down" | "close" | "focus_next" | "focus_prev" | "tab_next" | "tab_prev" | "focus_side";

const LABELS: Record<KeyProfile, Record<LabelledAction, string[]>> = {
  app: {
    new_tab: ["Cmd", "T"],
    split_right: ["Cmd", "D"],
    split_down: ["Cmd", "Shift", "D"],
    close: ["Cmd", "W"],
    focus_next: ["Cmd", "]"],
    focus_prev: ["Cmd", "["],
    tab_next: ["Cmd", "Shift", "]"],
    tab_prev: ["Cmd", "Shift", "["],
    focus_side: ["Cmd", "Option", "Arrow"],
  },
  web: {
    new_tab: ["Ctrl", "Shift", "Enter"],
    split_right: ["Ctrl", "Shift", "D"],
    split_down: ["Ctrl", "Shift", "E"],
    close: ["Ctrl", "Shift", "X"],
    focus_next: ["Ctrl", "Shift", "]"],
    focus_prev: ["Ctrl", "Shift", "["],
    tab_next: ["Ctrl", "Shift", "."],
    tab_prev: ["Ctrl", "Shift", ","],
    focus_side: ["Ctrl", "Shift", "Arrow"],
  },
};

const SYMBOLS: Record<string, string> = { Cmd: "⌘", Shift: "⇧", Ctrl: "⌃", Option: "⌥", Enter: "↵" };

export function shortcutLabel(action: LabelledAction, profile: KeyProfile): string {
  return LABELS[profile][action].join(" ");
}

export function shortcutKeycaps(action: LabelledAction, profile: KeyProfile): string {
  return LABELS[profile][action].map((key) => SYMBOLS[key] ?? key).join(" ");
}

const DESCRIBED: [LabelledAction, string][] = [
  ["new_tab", "New terminal tab"],
  ["split_right", "Split the terminal right"],
  ["split_down", "Split the terminal down"],
  ["close", "Close the focused terminal (the last tab asks first)"],
  ["focus_prev", "Previous terminal"],
  ["focus_next", "Next terminal"],
  ["focus_side", "Terminal on that side"],
  ["tab_prev", "Previous terminal tab"],
  ["tab_next", "Next terminal tab"],
];

export function terminalShortcutRows(profile: KeyProfile): [string[], string][] {
  return DESCRIBED.map(([action, text]) => [LABELS[profile][action].map((key) => SYMBOLS[key] ?? key), text]);
}

function macEdit(e: KeyboardEvent): string | null {
  if (e.ctrlKey) return null;
  if (e.key === "Backspace") return e.altKey ? "\x1b\x7f" : e.metaKey ? "\x15" : null;
  if (e.key === "ArrowLeft") return e.metaKey ? "\x01" : e.altKey ? "\x1bb" : null;
  if (e.key === "ArrowRight") return e.metaKey ? "\x05" : e.altKey ? "\x1bf" : null;
  return null;
}

export function bindTerminalKeys(term: Terminal, onShortcut?: (action: TerminalAction) => void, profile: KeyProfile = "web") {
  term.attachCustomKeyEventHandler((e) => {
    if (onShortcut) {
      const action = terminalShortcut(e, profile);
      if (action) {
        e.preventDefault();
        e.stopPropagation();
        if (e.type === "keydown") onShortcut(action);
        return false;
      }
    }
    if (e.key === "Tab" && e.shiftKey) return false;
    if (!e.metaKey && !e.altKey) return true;
    const seq = macEdit(e);
    if (seq === null) return true;
    e.preventDefault();
    if (e.type === "keydown") term.input(seq, true);
    return false;
  });
}
