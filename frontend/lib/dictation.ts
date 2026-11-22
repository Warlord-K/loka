export interface DictationAnchor {
  before: string;
  after: string;
}

export interface DictationPart {
  text: string;
  final: boolean;
}

export interface Spliced {
  value: string;
  start: number;
  provisionalStart: number;
  end: number;
}

const HUGS_LEFT = /^[\s.,!?;:)\]}'"]/;

export function joinWords(a: string, b: string): string {
  if (!a) return b;
  if (!b) return a;
  return /\s$/.test(a) || HUGS_LEFT.test(b) ? a + b : `${a} ${b}`;
}

export function anchorAt(value: string, from: number | null, to: number | null): DictationAnchor {
  const start = Math.max(0, Math.min(from ?? value.length, value.length));
  const end = Math.max(start, Math.min(to ?? start, value.length));
  return { before: value.slice(0, start), after: value.slice(end) };
}

export function spokenText(parts: DictationPart[]): { firm: string; provisional: string } {
  let firm = "";
  let provisional = "";
  let settled = true;
  for (const part of parts) {
    const text = part.text.trim();
    settled = settled && part.final;
    if (!text) continue;
    if (settled) firm = joinWords(firm, text);
    else provisional = joinWords(provisional, text);
  }
  return { firm, provisional };
}

export function splice(anchor: DictationAnchor, firm: string, provisional: string): Spliced {
  const spoken = joinWords(firm, provisional);
  const lead = anchor.before && spoken && !/\s$/.test(anchor.before) ? " " : "";
  const trail = anchor.after && spoken && !HUGS_LEFT.test(anchor.after) ? " " : "";
  const start = anchor.before.length + lead.length;
  const end = start + spoken.length;
  return {
    value: `${anchor.before}${lead}${spoken}${trail}${anchor.after}`,
    start,
    provisionalStart: provisional ? end - provisional.length : end,
    end,
  };
}

export interface CutRule {
  segmentMs: number;
  silentMs: number;
}

export function shouldCut({ segmentMs, silentMs }: CutRule): boolean {
  if (segmentMs >= 24000) return true;
  if (segmentMs >= 14000) return silentMs >= 200;
  return segmentMs >= 8000 && silentMs >= 500;
}
