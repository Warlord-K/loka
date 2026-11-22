export type TurnRole = "user" | "agent";

export type TurnLine = { role: TurnRole; key: string; text: string; partial?: boolean };

type Entry = TurnLine & { reservedAt?: number };

export const RESERVATION_MS = 8000;

const signature = (l: TurnLine) => `${l.partial ? 1 : 0}${l.text}`;

const line = ({ role, key, text, partial }: Entry): TurnLine => (partial ? { role, key, text, partial } : { role, key, text });

export class CallTurns {
  private lines = new Map<string, Entry>();
  private saved = new Map<string, string>();

  reserve(role: TurnRole, key: string, now = Date.now()): void {
    if (!this.lines.has(key)) this.lines.set(key, { role, key, text: "", reservedAt: now });
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
      if (this.saved.get(entry.key) !== signature(entry)) out.push(line(entry));
    }
    return out;
  }

  holding(): boolean {
    return [...this.lines.values()].some((entry) => entry.text === "");
  }

  markSaved(lines: TurnLine[]): void {
    for (const l of lines) this.saved.set(l.key, signature(l));
  }

  transcript(): TurnLine[] {
    return [...this.lines.values()].filter((entry) => entry.text !== "").map(line);
  }
}
