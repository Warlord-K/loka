export type ViewPhase = "starting" | "connecting" | "waiting" | "live" | "idle" | "ended" | "error";

export interface ViewFrame {
  src: string;
  width: number;
  height: number;
}

export interface ViewState {
  phase: ViewPhase;
  idle: "not_running" | "closed" | null;
  frame: ViewFrame | null;
  url: string | null;
  ended: string | null;
  note: string | null;
}

export interface BrowserHelpAsk {
  cardId: string;
  site: string;
  url?: string | null;
}

export interface WatchRequest {
  agentId: string;
  questId?: string | null;
  questTitle?: string | null;
  help?: BrowserHelpAsk | null;
}

export function viewTitle(agent: string, request: Pick<WatchRequest, "questId" | "questTitle">): string {
  if (!request.questId) return `${agent}'s browser`;
  const title = request.questTitle?.replace(/\s+/g, " ").trim();
  return title ? `${agent}'s browser for "${title}"` : `${agent}'s browser for one quest`;
}

export function viewKey(request: WatchRequest): string {
  return request.questId ? `${request.agentId}:${request.questId}` : request.agentId;
}

export const WATCH_BROWSER_EVENT = "world:watch-browser";
export const MAX_VIEWS = 4;

export function startView(): ViewState {
  return { phase: "starting", idle: null, frame: null, url: null, ended: null, note: null };
}

type Incoming = { type?: unknown; [key: string]: unknown };

const text = (value: unknown) => (typeof value === "string" ? value : "");
const count = (value: unknown) => (typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.round(value) : 0);

export function viewReducer(state: ViewState, message: Incoming): ViewState {
  if (state.phase === "ended") return state;
  switch (message.type) {
    case "status": {
      if (message.state !== "live") return { ...state, phase: state.frame ? state.phase : "connecting" };
      return state.phase === "starting" || state.phase === "connecting" ? { ...state, phase: "waiting" } : state;
    }
    case "frame": {
      const data = text(message.data);
      if (!data) return state;
      return { ...state, phase: "live", idle: null, note: null, frame: { src: `data:image/jpeg;base64,${data}`, width: count(message.width), height: count(message.height) } };
    }
    case "page":
      return { ...state, url: text(message.url) || state.url };
    case "idle":
      return { ...state, phase: "idle", idle: message.reason === "closed" ? "closed" : "not_running" };
    case "error":
      return { ...state, note: text(message.message) || null };
    case "bye":
    case "closed":
      return { ...state, phase: "ended", ended: text(message.reason) || "closed" };
    default:
      return state;
  }
}

export function pagePoint(client: { x: number; y: number }, box: { left: number; top: number; width: number; height: number }, frame: { width: number; height: number }): { x: number; y: number } | null {
  if (box.width <= 0 || box.height <= 0 || frame.width <= 0 || frame.height <= 0) return null;
  const scale = Math.min(box.width / frame.width, box.height / frame.height);
  const left = box.left + (box.width - frame.width * scale) / 2;
  const top = box.top + (box.height - frame.height * scale) / 2;
  const x = (client.x - left) / scale;
  const y = (client.y - top) / scale;
  if (x < 0 || y < 0 || x > frame.width || y > frame.height) return null;
  return { x: Math.round(x * 10) / 10, y: Math.round(y * 10) / 10 };
}

export function modifiers(e: { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }): number {
  return (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
}

export function mouseButton(button: number): "left" | "middle" | "right" | "none" {
  return button === 0 ? "left" : button === 1 ? "middle" : button === 2 ? "right" : "none";
}

export function keyFrame(
  kind: "down" | "up",
  e: { key: string; code: string; keyCode: number; altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean },
): Record<string, unknown> {
  const frame: Record<string, unknown> = { type: "key", event: kind === "down" ? "keyDown" : "keyUp", key: e.key, code: e.code, modifiers: modifiers(e) };
  if (e.keyCode > 0) frame.key_code = e.keyCode;
  if (kind === "down" && !e.ctrlKey && !e.metaKey) {
    if (e.key === "Enter") frame.text = "\r";
    else if ([...e.key].length === 1) frame.text = e.key;
  }
  return frame;
}

export function pageToReopen(view: ViewState, help: BrowserHelpAsk | null | undefined, reopened: boolean): string | null {
  if (reopened || view.phase !== "idle" || !help?.url) return null;
  return help.url;
}

export function withView(views: WatchRequest[], next: WatchRequest): WatchRequest[] {
  const key = viewKey(next);
  const existing = views.find((view) => viewKey(view) === key);
  const merged = { agentId: next.agentId, questId: next.questId ?? null, questTitle: next.questTitle ?? existing?.questTitle ?? null, help: next.help ?? existing?.help ?? null };
  return [...views.filter((view) => viewKey(view) !== key), merged].slice(-MAX_VIEWS);
}

export function idleLine(agent: string, reason: ViewState["idle"]): string {
  return reason === "closed"
    ? `${agent}'s browser closed. The view comes back by itself when ${agent} browses again.`
    : `${agent} is not browsing right now. The view starts by itself when ${agent} opens a page.`;
}

export function endedLine(reason: string | null): string {
  if (reason === "idle") return "Paused after a quiet spell.";
  if (reason === "access_withdrawn") return "Access to this device ended.";
  return "This view ended.";
}

export function siteLabel(site: string): string {
  const clean = site.replace(/\s+/g, " ").trim();
  return clean.length > 40 ? `${clean.slice(0, 39)}...` : clean;
}

export function watchBrowser(request: WatchRequest) {
  window.dispatchEvent(new CustomEvent<WatchRequest>(WATCH_BROWSER_EVENT, { detail: request }));
}
