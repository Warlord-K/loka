export type TurnRole = "user" | "agent";

export type TurnLine = { role: TurnRole; key: string; text: string; partial?: boolean };

type Entry = TurnLine & { reservedAt?: number };

export const RESERVATION_MS = 8000;

export const UTTERANCE_SETTLE_MS = 2500;

const signature = (l: TurnLine) => `${l.partial ? 1 : 0}${l.text}`;

const line = ({ role, key, text, partial }: Entry): TurnLine => (partial ? { role, key, text, partial } : { role, key, text });

export class CallTurns {
  private lines = new Map<string, Entry>();
  private saved = new Map<string, string>();
  private held = new Set<string>();

  reserve(role: TurnRole, key: string, now = Date.now()): void {
    if (!this.lines.has(key)) this.lines.set(key, { role, key, text: "", reservedAt: now });
  }

  hold(key: string): void {
    this.held.add(key);
  }

  close(key: string): void {
    this.held.delete(key);
  }

  release(key: string): void {
    if (this.lines.get(key)?.text === "") this.lines.delete(key);
  }

  set(role: TurnRole, key: string, text: string, partial = false): boolean {
    const t = text.trim();
    if (!t) return false;
    const was = this.lines.get(key);
    if (was && was.text === t && Boolean(was.partial) === partial) return false;
    this.lines.set(key, partial ? { role, key, text: t, partial } : { role, key, text: t });
    return true;
  }

  unsaved(now = Date.now(), hold = true): TurnLine[] {
    const out: TurnLine[] = [];
    for (const entry of [...this.lines.values()]) {
      if (entry.text === "") {
        if (hold && now - (entry.reservedAt ?? 0) < RESERVATION_MS) break;
        this.lines.delete(entry.key);
        continue;
      }
      if (hold && this.held.has(entry.key)) break;
      if (this.saved.get(entry.key) !== signature(entry)) out.push(line(entry));
    }
    return out;
  }

  holding(): boolean {
    return this.held.size > 0 || [...this.lines.values()].some((entry) => entry.text === "");
  }

  markSaved(lines: TurnLine[]): void {
    for (const l of lines) this.saved.set(l.key, signature(l));
  }

  transcript(): TurnLine[] {
    return [...this.lines.values()].filter((entry) => entry.text !== "").map(line);
  }
}

function words(text: string): string[] {
  return text
    .trim()
    .split(/\s+/)
    .map((raw) => raw.toLowerCase().replace(/[^\p{L}\p{N}]/gu, ""))
    .filter(Boolean);
}

function repeatsAndExtends(prev: string, next: string): boolean {
  const a = words(prev);
  const b = words(next);
  return a.length > 0 && b.length > a.length && a.every((w, i) => w === b[i]);
}

export function joinSegments(parts: string[]): string {
  let acc = "";
  for (const raw of parts) {
    const part = raw.trim();
    if (!part) continue;
    acc = !acc || repeatsAndExtends(acc, part) ? part : `${acc} ${part}`;
  }
  return acc;
}

export class ReplyWait {
  private waiting = false;

  replied(cut: boolean): void {
    this.waiting = !cut;
  }

  interrupted(): void {
    this.waiting = false;
  }

  heard(playing: boolean, responding: boolean): boolean {
    if (!this.waiting || playing || responding) return false;
    this.waiting = false;
    return true;
  }
}

export type Heard = { key: string; text: string; open: boolean };

export class CallUtterances {
  private current: string | null = null;
  private itemTurn = new Map<string, string>();
  private turnItems = new Map<string, string[]>();
  private itemText = new Map<string, string>();
  private spokenAt = new Map<string, number>();
  private turns: CallTurns;

  constructor(turns: CallTurns) {
    this.turns = turns;
  }

  isOpen(): boolean {
    return this.current !== null;
  }

  spoken(item: string): void {
    if (!this.spokenAt.has(item)) this.spokenAt.set(item, this.spokenAt.size);
  }

  hear(item: string, text: string): Heard | null {
    const t = text.trim();
    if (!t) return null;
    this.spoken(item);
    this.itemText.set(item, t);
    let key = this.itemTurn.get(item);
    if (!key) {
      if (!this.current) {
        this.current = `u-${item}`;
        this.turnItems.set(this.current, []);
        this.turns.hold(this.current);
      }
      key = this.current;
      this.itemTurn.set(item, key);
      this.turnItems.get(key)?.push(item);
    }
    const full = this.textOf(key);
    this.turns.set("user", key, full);
    return { key, text: full, open: key === this.current };
  }

  close(): Heard | null {
    const key = this.current;
    if (!key) return null;
    this.current = null;
    this.turns.close(key);
    return { key, text: this.textOf(key), open: false };
  }

  private textOf(key: string): string {
    const items = [...(this.turnItems.get(key) ?? [])].sort((a, b) => (this.spokenAt.get(a) ?? 0) - (this.spokenAt.get(b) ?? 0));
    return joinSegments(items.map((item) => this.itemText.get(item) ?? ""));
  }
}
