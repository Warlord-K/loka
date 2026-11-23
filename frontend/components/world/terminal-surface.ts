"use client";

import type { Terminal } from "@xterm/xterm";
import { ROWS_TO_READ, terminalQuote, type TerminalQuote, type TerminalRow } from "@/lib/terminal-context";
import { readyToType, typingFor, type Typed } from "@/lib/terminal-type";
import { spotAbove } from "./quote";

const ANSI = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white", "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite"] as const;

export const TERMINAL_FONTS = '"JetBrains Mono", "SF Mono", ui-monospace, Menlo, Monaco, monospace';

function token(style: CSSStyleDeclaration, name: string, alpha = ""): string | undefined {
  const parts = style.getPropertyValue(name).trim().split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((n) => !Number.isFinite(n))) return undefined;
  return `#${parts.map((n) => Math.round(n).toString(16).padStart(2, "0")).join("")}${alpha}`;
}

export function terminalTheme(): Record<string, string> {
  const style = getComputedStyle(document.documentElement);
  const picked: Record<string, string | undefined> = {
    background: token(style, "--w-term-bg"),
    foreground: token(style, "--w-term-text"),
    cursor: token(style, "--w-term-cursor"),
    cursorAccent: token(style, "--w-term-bg"),
    selectionBackground: token(style, "--w-term-select"),
    scrollbarSliderBackground: token(style, "--w-term-line", "99"),
    scrollbarSliderHoverBackground: token(style, "--w-term-dim", "66"),
    scrollbarSliderActiveBackground: token(style, "--w-term-dim", "99"),
  };
  ANSI.forEach((name, i) => {
    picked[name] = token(style, `--w-term-${i}`);
  });
  return Object.fromEntries(Object.entries(picked).filter((entry): entry is [string, string] => typeof entry[1] === "string"));
}

export function terminalFontFamily(): string {
  const loaded = getComputedStyle(document.documentElement).getPropertyValue("--font-term").trim();
  return loaded ? `${loaded}, ${TERMINAL_FONTS}` : TERMINAL_FONTS;
}

export async function loadTerminalFont(size: number): Promise<string> {
  const family = terminalFontFamily();
  if (family === TERMINAL_FONTS || !document.fonts) return family;
  try {
    await Promise.all([document.fonts.load(`${size}px ${family}`), document.fonts.load(`bold ${size}px ${family}`)]);
  } catch {
    return TERMINAL_FONTS;
  }
  return family;
}

export function terminalFontSize(): number {
  const root = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  return Math.min(18, Math.max(12, Math.round(root * 0.8125)));
}

export function reducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

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
