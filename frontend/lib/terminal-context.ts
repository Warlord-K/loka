export type TerminalRow = { text: string; wrapped: boolean };
export type TerminalQuote = { text: string; label: string; context?: string };

export const TERMINAL_LINES = 200;
export const ROWS_TO_READ = 2000;
export const QUOTE_TEXT_CHARS = 2000;
export const QUOTE_CONTEXT_CHARS = 4000;

const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f]/g;

function clean(text: string) {
  return text.replace(CONTROL, "").replace(/\s+$/, "");
}

export function attachesByDefault(agentOwner: string | null | undefined, meId: string | null | undefined): boolean {
  return !!meId && agentOwner === meId;
}

export function joinRows(rows: TerminalRow[]): string[] {
  const lines: string[] = [];
  for (const row of rows) {
    if (row.wrapped && lines.length) lines[lines.length - 1] += row.text;
    else lines.push(row.text);
  }
  const cleaned = lines.map(clean);
  while (cleaned.length && !cleaned[cleaned.length - 1]) cleaned.pop();
  while (cleaned.length && !cleaned[0]) cleaned.shift();
  return cleaned;
}

export function tailWithin(lines: string[], maxChars: number, maxLines = TERMINAL_LINES): string[] {
  const kept: string[] = [];
  let used = 0;
  for (let i = lines.length - 1; i >= 0 && kept.length < maxLines; i -= 1) {
    const cost = lines[i].length + (kept.length ? 1 : 0);
    if (used + cost > maxChars) {
      if (!kept.length) kept.unshift(lines[i].slice(lines[i].length - maxChars));
      break;
    }
    kept.unshift(lines[i]);
    used += cost;
  }
  return kept;
}

export function terminalQuote({ selection, rows, device }: { selection: string; rows: TerminalRow[]; device: string }): TerminalQuote | null {
  const lines = joinRows(rows);
  const recent = tailWithin(lines, QUOTE_CONTEXT_CHARS).join("\n");
  const picked = selection
    .split("\n")
    .map(clean)
    .join("\n")
    .trim();
  if (picked) {
    const text = picked.slice(0, QUOTE_TEXT_CHARS);
    return { text, label: `Terminal selection on ${device}`, context: recent && recent !== text ? recent : undefined };
  }
  const last = tailWithin(lines, QUOTE_TEXT_CHARS);
  if (!last.length) return null;
  const text = last.join("\n");
  const shown = recent.split("\n").length;
  return {
    text,
    label: `Recent terminal output on ${device} (last ${shown} ${shown === 1 ? "line" : "lines"})`,
    context: recent !== text ? recent : undefined,
  };
}
