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

export const BARGE_CONFIRM_MS = 1200;

export const ECHO_LIMIT = 2;

export class BargeGuard {
  private duckedAt: number | null = null;
  private streak = 0;
  private echoes = 0;
  private confirmed = 0;

  local(now: number): boolean {
    if (this.streak >= ECHO_LIMIT) return false;
    this.duckedAt ??= now;
    return true;
  }

  server(): void {
    if (this.duckedAt !== null) this.confirmed++;
    this.duckedAt = null;
    this.streak = 0;
  }

  expired(now: number): boolean {
    if (this.duckedAt === null || now - this.duckedAt < BARGE_CONFIRM_MS) return false;
    this.duckedAt = null;
    this.streak++;
    this.echoes++;
    return true;
  }

  ducking(): boolean {
    return this.duckedAt !== null;
  }

  cancel(): void {
    this.duckedAt = null;
  }

  stats(): { echoes: number; confirmed: number; local_off: boolean } {
    return { echoes: this.echoes, confirmed: this.confirmed, local_off: this.streak >= ECHO_LIMIT };
  }
}

export type SessionStep = "voice" | "transcription" | "turns";

export const TURN_SILENCE_MS = 1200;

export const TURN_MAX_MS = 30_000;

export const PREROLL_CHUNKS = 8;

export const SESSION_WAIT_MS = 6000;

export function sessionFallback(session: Record<string, unknown>, message: string, tried: Set<SessionStep>): { session: Record<string, unknown>; dropped: SessionStep } | null {
  const audio = (session.audio ?? {}) as Record<string, unknown>;
  const input = (audio.input ?? {}) as Record<string, unknown>;
  const hasVoice = typeof session.voice === "string" && session.voice !== "" && !tried.has("voice");
  const hasTranscription = input.transcription !== undefined && input.transcription !== null && !tried.has("transcription");
  const hasTurns = session.turn_detection !== undefined && session.turn_detection !== null && !tried.has("turns");
  const withoutVoice = () => {
    const { voice: _voice, ...rest } = session;
    return { session: rest, dropped: "voice" as const };
  };
  const withoutTurns = () => ({ session: { ...session, turn_detection: null }, dropped: "turns" as const });
  if (hasVoice && /voice/i.test(message)) return withoutVoice();
  if (hasTurns && /turn|silence|vad|detection|padding|threshold/i.test(message)) return withoutTurns();
  if (hasTranscription) {
    const { transcription: _transcription, ...kept } = input;
    return { session: { ...session, audio: { ...audio, input: kept } }, dropped: "transcription" };
  }
  if (hasVoice) return withoutVoice();
  if (hasTurns) return withoutTurns();
  return null;
}

export function serverTurnsApplied(sent: Record<string, unknown>, applied: Record<string, unknown>): boolean {
  const wanted = sent.turn_detection as Record<string, unknown> | null | undefined;
  const got = applied.turn_detection as Record<string, unknown> | null | undefined;
  if (!wanted || !got) return false;
  const want = typeof wanted.silence_duration_ms === "number" ? wanted.silence_duration_ms : 0;
  return typeof got.silence_duration_ms === "number" && got.silence_duration_ms >= want;
}

export type TurnStep<T> = { opened: boolean; flush: T[]; commit: boolean; respond: boolean };

export class ClientTurns<T> {
  private open = false;
  private openedAt = 0;
  private quietSince: number | null = null;
  private preroll: T[] = [];
  private sent = 0;
  private committed = false;

  audio(chunk: T): T[] {
    if (this.open) {
      this.sent++;
      return [chunk];
    }
    this.preroll.push(chunk);
    if (this.preroll.length > PREROLL_CHUNKS) this.preroll.shift();
    return [];
  }

  tick(now: number, speaking: boolean): TurnStep<T> {
    const none: TurnStep<T> = { opened: false, flush: [], commit: false, respond: false };
    if (speaking) {
      this.quietSince = null;
      if (!this.open) {
        this.open = true;
        this.openedAt = now;
        const flush = this.preroll;
        this.preroll = [];
        this.sent = flush.length;
        return { ...none, opened: true, flush };
      }
      if (now - this.openedAt >= TURN_MAX_MS && this.sent > 0) {
        this.openedAt = now;
        this.sent = 0;
        this.committed = true;
        return { ...none, commit: true };
      }
      return none;
    }
    if (!this.open) return none;
    this.quietSince ??= now;
    if (now - this.quietSince < TURN_SILENCE_MS) return none;
    const fresh = this.sent > 0;
    const heard = fresh || this.committed;
    this.close();
    return { ...none, commit: fresh, respond: heard };
  }

  isOpen(): boolean {
    return this.open;
  }

  close(): void {
    this.open = false;
    this.quietSince = null;
    this.preroll = [];
    this.sent = 0;
    this.committed = false;
  }
}

export type CallEvent = { type: string; agent_id: string | null; payload: Record<string, unknown> };

export function spokenUpdate(ev: CallEvent, agentId: string, quickId: string | null): string | null {
  if (ev.type !== "message.created" || ev.agent_id !== agentId || ev.payload.author_kind !== "agent") return null;
  const from = typeof ev.payload.quick_session_id === "string" ? ev.payload.quick_session_id : null;
  if (quickId ? from !== quickId : from || ev.payload.author_agent_id) return null;
  const body = typeof ev.payload.body === "string" ? ev.payload.body.trim() : "";
  return body || null;
}
