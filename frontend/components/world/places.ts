import type { Opens, Surface } from "@/lib/loka";
import type { Furniture, FurnitureState, RoomSpec } from "./interior";
import type { CivicId } from "./places-art";

export type { CivicId };

export interface CivicPlace {
  id: CivicId;
  name: string;
  slot: [number, number];
  surfaces: Surface[];
  district?: string;
}

export const CIVIC: CivicPlace[] = [
  { id: "library", name: "Library", slot: [16, 8], surfaces: ["sessions", "reports"], district: "hilltop" },
  { id: "observatory", name: "Observatory", slot: [34, 8], surfaces: ["feeds"], district: "hilltop" },
  { id: "clock", name: "Clock Tower", slot: [20, 19], surfaces: ["timers", "schedules"] },
  { id: "market", name: "Market", slot: [54, 19], surfaces: ["usage"] },
  { id: "post", name: "Post Office", slot: [18, 31], surfaces: ["inbox"] },
  { id: "forge", name: "Forge", slot: [34, 8], surfaces: ["workshop", "terminal", "devices"], district: "workshop" },
];

export const SURFACES: Surface[] = ["timers", "schedules", "sessions", "workshop", "terminal", "devices", "inbox", "usage", "feeds", "reports"];

const OPENS_KINDS = new Set(["surface", "channel", "quest", "list", "challenge", "region", "url"]);

export function parseOpens(value: unknown): Opens | null {
  if (!value || typeof value !== "object") return null;
  const { kind, ref } = value as { kind?: unknown; ref?: unknown };
  if (typeof kind !== "string" || typeof ref !== "string" || !OPENS_KINDS.has(kind) || !ref.trim()) return null;
  if (kind === "surface" && !SURFACES.includes(ref as Surface)) return null;
  if (kind === "url" && !/^https?:\/\//i.test(ref)) return null;
  return { kind, ref: ref.trim() } as Opens;
}

export type PlaceAction =
  | { kind: "surface"; surface: Surface }
  | { kind: "agent"; agentId: string }
  | { kind: "skills"; agentId: string }
  | { kind: "notes"; agentId: string }
  | { kind: "notes-all" }
  | { kind: "kit"; agentId: string }
  | { kind: "workbench"; agentId: string }
  | { kind: "history"; agentId: string | null }
  | { kind: "chat" }
  | { kind: "shell" }
  | { kind: "slack" }
  | { kind: "room"; room: string }
  | { kind: "academy"; challengeId?: string | null }
  | { kind: "npc"; id: string };

export interface PlaceRoom {
  spec: RoomSpec;
  actions: Record<string, PlaceAction>;
}

export interface CivicContext {
  agents: { id: string; name: string }[];
  devices: { id: string; name: string; state: "busy" | "online" | "offline" }[];
  unread: number;
  chatUnread: number;
  timers: number;
  budgetAlarm: boolean;
  runs: number;
}

export interface HouseContext {
  agentId: string;
  name: string;
  skills: { id: string; name: string }[];
  servers: { id: string; name: string; enabled: boolean }[];
  hasNotes: boolean;
  workshops: boolean;
  busy: boolean;
}

function hue(seed: string): string {
  let h = 7;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  return ["#b8513a", "#3f6fb0", "#2f7a5a", "#e8b64a", "#6c4f9e", "#c0603a", "#2f8a8a", "#8f3f35"][h % 8];
}

function books(seed: string, n: number): string[] {
  return Array.from({ length: n }, (_, i) => hue(`${seed}:${i}`));
}

function room(id: string, title: string, floor: RoomSpec["floor"], wall: string, items: [Furniture, PlaceAction | null][], w = 13, h = 10): PlaceRoom {
  const actions: Record<string, PlaceAction> = {};
  for (const [f, a] of items) if (a && f.label) actions[f.id] = a;
  return { spec: { id, title, w, h, floor, wall, furniture: items.map(([f]) => f) }, actions };
}

const DEVICE_STATE: Record<CivicContext["devices"][number]["state"], FurnitureState> = { busy: "busy", online: "on", offline: "off" };

export function civicRoom(id: CivicId, ctx: CivicContext): PlaceRoom {
  const s = (surface: Surface): PlaceAction => ({ kind: "surface", surface });
  const plants: [Furniture, null][] = [
    [{ id: "plant-a", kind: "plant", x: 1, y: 8 }, null],
    [{ id: "plant-b", kind: "plant", x: 11, y: 8 }, null],
  ];
  if (id === "clock") {
    return room("civic:clock", "Clock Tower", "stone", "#cdbd9f", [
      [{ id: "clock", kind: "clock", x: 6, y: 2, label: ctx.timers ? `Timers (${ctx.timers})` : "Timers" }, s("timers")],
      [{ id: "ledger", kind: "desk", x: 2, y: 2, label: "Schedules", tint: "#2f7a5a" }, s("schedules")],
      [{ id: "drum", kind: "drum", x: 10, y: 2, label: "Heartbeats" }, s("schedules")],
      [{ id: "rug", kind: "rug", x: 5, y: 4, tint: "#2f7a5a" }, null],
      ...plants,
    ]);
  }
  if (id === "library") {
    const shelves = ctx.agents.slice(0, 3);
    return room("civic:library", "Library", "wood", "#e6dcc6", [
      ...shelves.map((a, i): [Furniture, PlaceAction] => [
        { id: `shelf-${a.id}`, kind: "shelf", x: 1 + i * 3, y: 2, label: `${a.name}'s sessions`, colors: books(a.id, 18) },
        { kind: "history", agentId: a.id },
      ]),
      ...(shelves.length < 3
        ? [[{ id: "shelf-empty", kind: "shelf", x: 1 + shelves.length * 3, y: 2, colors: books("empty", 6) }, null] as [Furniture, null]]
        : []),
      [{ id: "notes", kind: "chest", x: 11, y: 2, label: "Agents' notes" }, { kind: "notes-all" }],
      [{ id: "reading", kind: "desk", x: 5, y: 5, label: "Every session and rewind", tint: "#6c4f9e" }, { kind: "history", agentId: null }],
      [{ id: "lamp", kind: "lamp", x: 10, y: 5 }, null],
      [{ id: "reports", kind: "desk", x: 1, y: 6, label: "Reports", tint: "#2f7a5a" }, s("reports")],
      ...plants,
    ]);
  }
  if (id === "forge") {
    const devices = ctx.devices.slice(0, 5);
    return room("civic:forge", "Forge", "stone", "#8e8279", [
      [{ id: "terminal", kind: "terminal", x: 1, y: 2, label: "Terminal", state: ctx.devices.some((d) => d.state !== "offline") ? "on" : "off" }, s("terminal")],
      [{ id: "bench", kind: "workbench", x: 4, y: 2, label: "Workshop" }, s("workshop")],
      [{ id: "hearth", kind: "hearth", x: 7, y: 2 }, null],
      [{ id: "anvil", kind: "anvil", x: 8, y: 5, label: "Shell runs" }, { kind: "shell" }],
      ...(devices.length
        ? devices.map((d, i): [Furniture, PlaceAction] => [
            { id: `device-${d.id}`, kind: "machine", x: 11, y: 2 + i, label: `${d.name} (${d.state})`, state: DEVICE_STATE[d.state] },
            s("devices"),
          ])
        : [[{ id: "device-none", kind: "machine", x: 11, y: 2, label: "Add a device", state: "off" }, s("devices")] as [Furniture, PlaceAction]]),
      [{ id: "plant-a", kind: "plant", x: 1, y: 8 }, null],
    ]);
  }
  if (id === "post") {
    return room("civic:post", "Post Office", "tile", "#eef0f4", [
      [{ id: "mail", kind: "mailbox", x: 1, y: 2, label: ctx.chatUnread ? `Messages (${ctx.chatUnread})` : "Messages", colors: books("mail", Math.min(24, ctx.chatUnread)) }, { kind: "chat" }],
      [{ id: "counter", kind: "counter", x: 5, y: 4, label: ctx.unread ? `Inbox (${ctx.unread})` : "Inbox", colors: books("inbox", Math.min(6, ctx.unread)) }, s("inbox")],
      [{ id: "slack", kind: "desk", x: 9, y: 2, label: "Slack", tint: "#6c4f9e" }, { kind: "slack" }],
      ...plants,
    ]);
  }
  if (id === "market") {
    return room("civic:market", "Market", "wood", "#f0e2c2", [
      [{ id: "counter", kind: "counter", x: 2, y: 2, label: "Usage and cost", colors: ["#e85d40", "#f2c14e", "#7bc74d", "#9b7be0"] }, s("usage")],
      [{ id: "scale", kind: "scale", x: 7, y: 2, label: "Daily budgets" }, s("usage")],
      [{ id: "lamp", kind: "lamp", x: 10, y: 2, label: ctx.budgetAlarm ? "Budget warning" : "Budget lamp", state: ctx.budgetAlarm ? "alarm" : "on" }, s("usage")],
      [{ id: "chest", kind: "chest", x: 10, y: 5 }, null],
      [{ id: "rug", kind: "rug", x: 4, y: 5, tint: "#2f9a8a" }, null],
      ...plants,
    ]);
  }
  return room("civic:observatory", "Observatory", "stone", "#c5cbd8", [
    [{ id: "telescope", kind: "telescope", x: 5, y: 3, label: "Feeds" }, s("feeds")],
    [{ id: "chart-a", kind: "chart", x: 1, y: 2 }, null],
    [{ id: "chart-b", kind: "chart", x: 10, y: 2 }, null],
    [{ id: "rug", kind: "rug", x: 4, y: 6, tint: "#1d2448" }, null],
  ]);
}

export function houseRoom(ctx: HouseContext): PlaceRoom {
  const servers = ctx.servers.slice(0, 3);
  const items: [Furniture, PlaceAction | null][] = [
    [
      { id: "shelf", kind: "shelf", x: 1, y: 2, label: ctx.skills.length ? `Skills (${ctx.skills.length})` : "Skills", colors: ctx.skills.map((k) => hue(k.name)) },
      { kind: "skills", agentId: ctx.agentId },
    ],
    [{ id: "notes", kind: "desk", x: 4, y: 2, label: ctx.hasNotes ? "Notes" : "Notes (empty)" }, { kind: "notes", agentId: ctx.agentId }],
    ...servers.map((m, i): [Furniture, PlaceAction] => [
      { id: `mcp-${m.id}`, kind: "machine", x: 7 + i, y: 2, label: i === 2 && ctx.servers.length > 3 ? `MCP servers (${ctx.servers.length})` : `MCP: ${m.name}`, state: m.enabled ? (ctx.busy ? "busy" : "on") : "off", tint: hue(m.name) },
      { kind: "kit", agentId: ctx.agentId },
    ]),
    ...(servers.length ? [] : [[{ id: "mcp-none", kind: "machine", x: 8, y: 2, label: "MCP servers", state: "off" }, { kind: "kit", agentId: ctx.agentId }] as [Furniture, PlaceAction]]),
    [{ id: "bell", kind: "bell", x: 3, y: 6, label: `Talk to ${ctx.name}` }, { kind: "agent", agentId: ctx.agentId }],
    [{ id: "bed", kind: "bed", x: 8, y: 5, tint: hue(ctx.agentId) }, null],
    [{ id: "rug", kind: "rug", x: 4, y: 4, tint: hue(`${ctx.agentId}:rug`) }, null],
    [{ id: "plant", kind: "plant", x: 9, y: 7 }, null],
  ];
  if (ctx.workshops) items.push([{ id: "bench", kind: "workbench", x: 1, y: 5, label: "Open the workshop" }, { kind: "workbench", agentId: ctx.agentId }]);
  return room(`house:${ctx.agentId}`, `${ctx.name}'s house`, "wood", "#f3e6c8", items, 11, 9);
}
