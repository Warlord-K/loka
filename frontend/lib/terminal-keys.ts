import type { Terminal } from "@xterm/xterm";

function macEdit(e: KeyboardEvent): string | null {
  if (e.ctrlKey) return null;
  if (e.key === "Backspace") return e.altKey ? "\x1b\x7f" : e.metaKey ? "\x15" : null;
  if (e.key === "ArrowLeft") return e.metaKey ? "\x01" : e.altKey ? "\x1bb" : null;
  if (e.key === "ArrowRight") return e.metaKey ? "\x05" : e.altKey ? "\x1bf" : null;
  return null;
}

export function bindTerminalKeys(term: Terminal) {
  term.attachCustomKeyEventHandler((e) => {
    if (e.key === "Tab" && e.shiftKey) return false;
    if (!e.metaKey && !e.altKey) return true;
    const seq = macEdit(e);
    if (seq === null) return true;
    e.preventDefault();
    if (e.type === "keydown") term.input(seq, true);
    return false;
  });
}
