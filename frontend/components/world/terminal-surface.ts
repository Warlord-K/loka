"use client";

import type { Terminal } from "@xterm/xterm";
import { ROWS_TO_READ, terminalQuote, type TerminalQuote, type TerminalRow } from "@/lib/terminal-context";
import { readyToType, typingFor, type Typed } from "@/lib/terminal-type";
import { spotAbove } from "./quote";

export const TERMINAL_THEME = {
  background: "#120c15",
  foreground: "#e9dcc6",
  cursor: "#e8b64a",
  cursorAccent: "#120c15",
  selectionBackground: "#3d2f4a",
  black: "#1d1422",
  red: "#e07a5f",
  green: "#7be06b",
  yellow: "#e8b64a",
  blue: "#6bc7e8",
  magenta: "#c39bff",
  cyan: "#19b8b8",
  white: "#e9dcc6",
  brightBlack: "#5f4f60",
  brightRed: "#ff8fa8",
  brightGreen: "#9bf08b",
  brightYellow: "#f2c66a",
  brightBlue: "#9fdcff",
  brightMagenta: "#d9bfff",
  brightCyan: "#6be0e0",
  brightWhite: "#fff8ea",
};

export function bufferRows(t: Terminal): TerminalRow[] {
  const buffer = t.buffer.active;
  const rows: TerminalRow[] = [];
  for (let i = Math.max(0, buffer.length - ROWS_TO_READ); i < buffer.length; i += 1) {
    const line = buffer.getLine(i);
    if (line) rows.push({ text: line.translateToString(true), wrapped: line.isWrapped });
  }
  return rows;
}

export function readTerminal(t: Terminal, selectionOnly: boolean, device: string): TerminalQuote | null {
  const selection = t.hasSelection() ? t.getSelection() : "";
  if (selectionOnly && !selection.trim()) return null;
  return terminalQuote({ selection, rows: bufferRows(t), device });
}

export function selectionSpot(t: Terminal, screen: Element | null | undefined) {
  const range = t.getSelectionPosition();
  if (!range || !screen) return null;
  const rect = screen.getBoundingClientRect();
  const cellW = rect.width / t.cols;
  const cellH = rect.height / t.rows;
  const top = t.buffer.active.viewportY;
  const first = range.start.y - top;
  const last = range.end.y - top;
  if (last < 0 || first > t.rows - 1) return null;
  const from = Math.max(first, 0);
  const to = Math.min(last, t.rows - 1);
  const oneRow = first === last;
  return spotAbove({
    left: rect.left + (oneRow ? Math.min(range.start.x, range.end.x) : 0) * cellW,
    width: (oneRow ? Math.abs(range.end.x - range.start.x) : t.cols) * cellW,
    top: rect.top + from * cellH,
    bottom: rect.top + (to + 1) * cellH,
  });
}

const POLL_MS = 100;

export function typeCommand(t: Terminal, send: (data: string) => void, command: string, at: { liveSince: number; alive: () => boolean }): Promise<Typed> {
  let lastOutput = Date.now();
  const parsed = t.onWriteParsed(() => {
    lastOutput = Date.now();
  });
  return new Promise<Typed>((resolve) => {
    const finish = (result: Typed) => {
      parsed.dispose();
      resolve(result);
    };
    const tick = () => {
      if (!at.alive()) return finish("cancelled");
      const now = Date.now();
      const bracketed = t.modes.bracketedPasteMode;
      if (!readyToType({ bracketed, sinceLive: now - at.liveSince, sinceOutput: now - lastOutput })) {
        window.setTimeout(tick, POLL_MS);
        return;
      }
      const typing = typingFor(command, bracketed);
      if (typing.kind === "empty") return finish("empty");
      if (typing.kind === "copy") {
        const copying = navigator.clipboard?.writeText(typing.text);
        if (!copying) return finish("not_copied");
        void copying.then(
          () => finish("copied"),
          () => finish("not_copied"),
        );
        return;
      }
      send(typing.data);
      t.focus();
      finish("typed");
    };
    tick();
  });
}
