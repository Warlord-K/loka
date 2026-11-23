import type { RegionDetail } from "./loka";
import type { WorldItem } from "./world";

export type ExactWorldLink = { kind: "team" | "list" | "place" | "region"; id: string };

export type ExactMapDestination = {
  regionId: string;
  region: RegionDetail | null;
  place: WorldItem | null;
  label: string;
};

export type ExactDestination = ExactWorldLink & { map?: ExactMapDestination };

export type NavigationKind = "agent" | "quest" | "report" | "canvas" | "team" | "list" | "todo" | "person" | "channel" | "message" | "reply" | "region" | "place" | "practice" | "lesson" | "academy";

export type NavigationForeground = {
  panel: "agent" | "history" | "reports" | "board" | "lists" | "people" | "chat" | "map" | "academy";
  board?: "teams";
  keepSideChat?: boolean;
  leaveInterior?: boolean;
  closeTerminal?: boolean;
  overlay?: boolean;
};

const foreground = (policy: NavigationForeground): NavigationForeground => policy;

const FOREGROUND: Record<NavigationKind, NavigationForeground> = {
  agent: foreground({ panel: "agent" }),
  quest: foreground({ panel: "history" }),
  report: foreground({ panel: "reports", keepSideChat: true, overlay: true }),
  canvas: foreground({ panel: "reports", keepSideChat: true, overlay: true }),
  team: foreground({ panel: "board", board: "teams", closeTerminal: true }),
  list: foreground({ panel: "lists", keepSideChat: true, closeTerminal: true }),
  todo: foreground({ panel: "lists", keepSideChat: true }),
  person: foreground({ panel: "people" }),
  channel: foreground({ panel: "chat" }),
  message: foreground({ panel: "chat" }),
  reply: foreground({ panel: "chat" }),
  region: foreground({ panel: "map", leaveInterior: true, closeTerminal: true }),
  place: foreground({ panel: "map", leaveInterior: true, closeTerminal: true }),
  practice: foreground({ panel: "academy", keepSideChat: true }),
  lesson: foreground({ panel: "academy", keepSideChat: true }),
  academy: foreground({ panel: "academy", keepSideChat: true }),
};

export function navigationForeground(kind: NavigationKind): NavigationForeground {
  return FOREGROUND[kind];
}

export type NavigationTransition = NavigationForeground & {
  keepConversations: boolean;
  showSideChat: boolean;
  closeTerminal: boolean;
};

export function navigationTransition(
  kind: NavigationKind,
  current: { wide: boolean; sideChatSource: boolean },
  loadedList = false,
): NavigationTransition {
  const policy = navigationForeground(kind);
  return {
    ...policy,
    keepConversations: policy.panel !== "map",
    showSideChat: Boolean(policy.keepSideChat && current.wide && current.sideChatSource),
    closeTerminal: Boolean(policy.closeTerminal && !(kind === "list" && loadedList)),
  };
}

export function applyNavigationTransition(
  kind: NavigationKind,
  current: { wide: boolean; sideChatSource: boolean },
  actions: {
    closePanels: (keepConversations: boolean) => void;
    showSideChat: () => void;
    closeTerminal: () => void;
    showBoard: (board: "teams") => void;
  },
  loadedList = false,
) {
  const transition = navigationTransition(kind, current, loadedList);
  if (transition.showSideChat) actions.showSideChat();
  if (!transition.overlay) actions.closePanels(transition.keepConversations);
  if (transition.closeTerminal) actions.closeTerminal();
  if (transition.board) actions.showBoard(transition.board);
  return transition;
}

export type NavigationApplication = {
  kind: NavigationKind | "surface" | "chat-agent" | "chat-person";
};

export type NavigationApplicationOptions = {
  keepSideChat?: boolean;
  preservePanels?: boolean;
  loadedList?: boolean;
  messageTarget?: "agent" | "channel";
  threadedMessage?: boolean;
};

export type NavigationApplicationActions<T extends NavigationApplication> = {
  closePanels: (keepConversations: boolean) => void;
  showSideChat: () => void;
  closeTerminal: () => void;
  showBoard: (board: "teams") => void;
  prepare: () => void;
  surface: (destination: T & { kind: "surface" }) => void;
  agent: (destination: T & { kind: "agent" }) => void;
  quest: (destination: T & { kind: "quest" }) => void;
  reports: (destination: T & { kind: "report" | "canvas" }) => void;
  academy: (destination: T & { kind: "academy" | "practice" | "lesson" }) => void;
  todo: (destination: T & { kind: "todo" }) => void;
  person: (destination: T & { kind: "person" }) => void;
  channel: (destination: T & { kind: "channel" | "chat-person" }) => void;
  chatAgent: (destination: T & { kind: "chat-agent" }) => void;
  messageAgent: (destination: T & { kind: "message" }) => void;
  messageChannel: (destination: T & { kind: "message" }) => void;
  message: (destination: T & { kind: "message" }) => void;
  team: (destination: T & { kind: "team" }) => void;
  list: (destination: T & { kind: "list" }) => void;
  map: (destination: T & { kind: "place" | "region" }) => void;
};

export function applyNavigationDestination<T extends NavigationApplication>(
  destination: T,
  current: { wide: boolean; sideChatSource: boolean },
  actions: NavigationApplicationActions<T>,
  options: NavigationApplicationOptions = {},
) {
  if (destination.kind === "surface" && options.preservePanels) {
    actions.surface(destination as T & { kind: "surface" });
    return null;
  }
  const policyKind = destination.kind === "chat-agent" || destination.kind === "chat-person"
    ? "channel"
    : destination.kind === "message" && options.threadedMessage
      ? "reply"
      : destination.kind;
  const transition = policyKind === "surface"
    ? null
    : applyNavigationTransition(policyKind, current, actions, options.loadedList);
  if (!transition) {
    if (options.keepSideChat && current.wide) actions.showSideChat();
    actions.closePanels(true);
  }
  if (!transition?.overlay) actions.prepare();
  if (destination.kind === "surface") actions.surface(destination as T & { kind: "surface" });
  else if (destination.kind === "agent") actions.agent(destination as T & { kind: "agent" });
  else if (destination.kind === "quest") actions.quest(destination as T & { kind: "quest" });
  else if (destination.kind === "report" || destination.kind === "canvas") actions.reports(destination as T & { kind: "report" | "canvas" });
  else if (destination.kind === "academy" || destination.kind === "practice" || destination.kind === "lesson") actions.academy(destination as T & { kind: "academy" | "practice" | "lesson" });
  else if (destination.kind === "todo") actions.todo(destination as T & { kind: "todo" });
  else if (destination.kind === "person") actions.person(destination as T & { kind: "person" });
  else if (destination.kind === "channel" || destination.kind === "chat-person") actions.channel(destination as T & { kind: "channel" | "chat-person" });
  else if (destination.kind === "chat-agent") actions.chatAgent(destination as T & { kind: "chat-agent" });
  else if (destination.kind === "message") {
    const message = destination as T & { kind: "message" };
    if (options.messageTarget === "agent") actions.messageAgent(message);
    else if (options.messageTarget === "channel") actions.messageChannel(message);
    actions.message(message);
  } else if (destination.kind === "team") actions.team(destination as T & { kind: "team" });
  else if (destination.kind === "list") actions.list(destination as T & { kind: "list" });
  else if (destination.kind === "place" || destination.kind === "region") actions.map(destination as T & { kind: "place" | "region" });
  return transition;
}

export type WindowLayer = "terminal" | "agent" | "chat";

export function raiseWindowLayer(order: WindowLayer[], layer: WindowLayer): WindowLayer[] {
  return order.at(-1) === layer ? order : [...order.filter((item) => item !== layer), layer];
}

export function foregroundWindowLayer(order: WindowLayer[], open: Record<WindowLayer, boolean>) {
  return order.findLast((layer) => open[layer]);
}

export type ConversationSurface = "chat" | "agent" | "side" | "workbench";

export type ConversationForeground = {
  documentVisible: boolean;
  top: string;
  mobileList?: boolean;
  wide?: boolean;
  sideMounted?: boolean;
  workbenchOpen?: boolean;
};

export function conversationIsForeground(state: ConversationForeground, surface: ConversationSurface) {
  if (!state.documentVisible) return false;
  if (surface === "chat") return state.top === "chat" && !state.mobileList;
  if (surface === "agent") return state.top.startsWith("agent:");
  if (surface === "side") return Boolean(state.wide && state.sideMounted && state.top !== "covered" && state.top !== "workbench");
  return Boolean(state.workbenchOpen && state.top === "workbench");
}

export function conversationIsForegroundNow(state: ConversationForeground, surface: ConversationSurface) {
  const documentVisible = typeof document === "undefined" ? state.documentVisible : document.visibilityState === "visible";
  return conversationIsForeground({ ...state, documentVisible }, surface);
}

export function consumeMessageFocus(last: { current: number | undefined }, nonce: number) {
  if (last.current === nonce) return false;
  last.current = nonce;
  return true;
}

export function navigationRequests() {
  let latest = 0;
  return {
    begin() {
      const request = ++latest;
      return { request, current: () => request === latest };
    },
    cancel() {
      latest += 1;
    },
    current(request: number | undefined) {
      return request === undefined || request === latest;
    },
  };
}

export type NavigationRequest = { request: number; current: () => boolean };

export type DetailRead<T> =
  | { status: "ready"; value: T }
  | { status: "error"; error: unknown }
  | { status: "stale" };

export type GuardedDetailIntent = ReturnType<ReturnType<typeof guardedDetailLoader>["begin"]>;

export function guardedDetailLoader() {
  let latest = 0;
  return {
    begin(current: () => boolean = () => true) {
      const request = ++latest;
      const isCurrent = () => request === latest && current();
      return {
        current: isCurrent,
        async read<T>(load: () => Promise<T>): Promise<DetailRead<T>> {
          if (!isCurrent()) return { status: "stale" };
          try {
            const value = await load();
            return isCurrent() ? { status: "ready", value } : { status: "stale" };
          } catch (error) {
            return isCurrent() ? { status: "error", error } : { status: "stale" };
          }
        },
      };
    },
    invalidate() {
      latest += 1;
    },
  };
}

export async function runNavigation<T>(
  current: () => boolean,
  resolve: () => T | null | Promise<T | null>,
  allow: () => boolean | void | Promise<boolean | void>,
  apply: (destination: T) => void,
  unavailable: () => void,
) {
  let destination;
  try {
    destination = await resolve();
  } catch {
    if (current()) unavailable();
    return;
  }
  if (destination === null || !current() || (await allow()) === false || !current()) return;
  apply(destination);
}

export async function resolveMessageJump<Id extends string | number, Message extends { id: Id; reply_to?: Id | null }>(
  conversation: string,
  messageId: Id,
  replies: (conversation: string, messageId: Id) => Promise<Message[]>,
) {
  const rows = await replies(conversation, messageId);
  const message = rows.find((row) => row.id === messageId);
  if (!message) throw new Error("message unavailable");
  const root = message.reply_to;
  if (root == null) return { message };
  if (!rows.some((row) => row.id === root && row.reply_to == null)) throw new Error("thread unavailable");
  return { thread: { root, reply: messageId } };
}

export async function resolveExactDestination(
  link: ExactWorldLink,
  read: {
    team: (id: string) => Promise<{ id: string }>;
    item: (id: string) => Promise<WorldItem>;
    region: (id: string) => Promise<RegionDetail>;
    home: string;
  },
  isCurrent: () => boolean,
): Promise<ExactDestination | null> {
  if (link.kind === "team") {
    const team = await read.team(link.id);
    return isCurrent() ? { kind: link.kind, id: team.id } : null;
  }
  if (link.kind === "region") {
    const region = await read.region(link.id);
    return isCurrent() ? { kind: link.kind, id: region.region.id, map: { regionId: region.region.id, region, place: null, label: region.region.name } } : null;
  }
  const item = await read.item(link.id);
  if (!isCurrent()) return null;
  if (link.kind === "list") {
    if (item.kind !== "list" && item.kind !== "timer") throw new Error("wrong kind");
    return { kind: link.kind, id: item.id };
  }
  if (item.kind !== "building" && item.kind !== "npc") throw new Error("wrong kind");
  const regionId = item.region_id ?? read.home;
  const region = regionId === read.home ? null : await read.region(regionId);
  return isCurrent() ? { kind: link.kind, id: item.id, map: { regionId, region, place: item, label: item.name } } : null;
}

export function revealExactMap(
  destination: ExactMapDestination,
  currentRegion: string,
  engine: { currentPlace: () => string | null; leaveRoom: (instant: boolean) => void } | null,
  travel: (destination: ExactMapDestination) => void,
  reveal: (place: WorldItem | null) => void,
): void {
  if (destination.regionId !== currentRegion) {
    travel(destination);
    return;
  }
  if (!engine) return;
  if (engine.currentPlace()) engine.leaveRoom(true);
  reveal(destination.place);
}
