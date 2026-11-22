"use client";

import { useEffect, useState, type ReactNode } from "react";
import { ArrowSquareOut, Books, Buildings, CheckSquare, FileText, FlowArrow, Hash, ListChecks, MapTrifold, Square, Student, Timer, UserCircle, UsersThree } from "@phosphor-icons/react";
import { latestOnly } from "@/lib/latest";
import { cn, safeDecode } from "@/lib/utils";
import { messageTime, dayLabel, worldApi, type ChannelSummary, type TaskBrief, type Todo, type WorldAgent, type WorldEvent } from "@/lib/world";
import { AgentFace } from "./channel-ui";
import { MessageLink, messageRefFromHref, openWorldLink, worldRefFromHref } from "./message-links";
import { SetupCard } from "./setup-hint";
import { SlackMark } from "./panels";
import { StatusIcon, statusLabel } from "./quest-ui";
import { countdown, focusTodo, isTimerTodo } from "./todos-ui";
import { canvasItemKind, canvasItemLabel, openReport, REPORTS_STALE, reportsApi, type CanvasItem, type ReportBrief } from "@/lib/reports";
import { ATTEMPT_ID, ATTEMPT_ID_SOURCE, checkOf, openPractice, practiceApi, subjectLabel, type AttemptCard } from "@/lib/practice";
import { teamsApi, type Team } from "@/lib/teams";
import type { WorldItem } from "@/lib/world";
import { openAcademy, type AdventureCard, type ChallengeDetail, type RegionDetail } from "@/lib/loka";
import { lokaApi } from "./loka-client";

const REF_SOURCE = "ch\\d{1,18}|[0-9a-fA-F][0-9a-fA-F-]{5,35}";
const ID_SOURCE = "[0-9a-fA-F][0-9a-fA-F-]{5,35}";
const ID = new RegExp(`^(?:${ID_SOURCE})$`);
const EXACT_ID_SOURCE = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
const EXACT_ID = new RegExp(`^(?:${EXACT_ID_SOURCE})$`);
const ITEM_SOURCE = "#[A-Za-z0-9_-]{1,40}";

function matchesId(id: string, ref: string): boolean {
  return id.replace(/-/g, "").startsWith(ref.replace(/-/g, ""));
}

export type Embed =
  | { kind: "msg"; ref: string }
  | { kind: "todo"; ref: string }
  | { kind: "quest"; ref: string }
  | { kind: "report"; ref: string; item?: string }
  | { kind: "practice"; ref: string }
  | { kind: "team" | "list" | "place" | "region"; ref: string }
  | { kind: "challenge" | "course"; ref: string }
  | { kind: "agent"; ref: string }
  | { kind: "setup"; key: string }
  | { kind: "channel"; id: string }
  | { kind: "slack"; url: string; workspace: string; thread: boolean }
  | { kind: "linear"; url: string; key: string; title: string }
  | { kind: "notion"; url: string; title: string; workspace: string; page: string; published: boolean };

const SLACK = /^https:\/\/([a-z0-9-]+)\.slack\.com\/archives\/[A-Z0-9]+\/p\d{10,}/i;
const NOTION = /^https:\/\/(?:www\.)?(?:([a-z0-9-]+)\.notion\.site|notion\.so)\/(?:([^/?#\s]+)\/)?([^/?#\s]*?)-?([0-9a-f]{32})(?:[/?#]|$)/i;
const LINEAR = /^https:\/\/linear\.app\/[^/\s]+\/issue\/([A-Z][A-Z0-9]*-\d+)(?:\/([^/?#\s]+))?/i;

function titleFromSlug(slug?: string): string {
  if (!slug) return "";
  const words = safeDecode(slug).split("-").filter(Boolean);
  const text = words.join(" ");
  return text ? text[0].toUpperCase() + text.slice(1) : "";
}

export function embedFromHref(href?: string | null): Embed | null {
  if (!href) return null;
  const msg = messageRefFromHref(href);
  if (msg) return { kind: "msg", ref: msg };
  const linked = worldRefFromHref(href);
  if (linked) return linked;
  const setup = href.match(SETUP);
  if (setup) return { kind: "setup", key: setup[1].toLowerCase().replace(/-/g, "_") };
  const item = href.match(new RegExp(`^report:(${ID_SOURCE})(${ITEM_SOURCE})$`, "i"));
  if (item) return { kind: "report", ref: item[1].toLowerCase(), item: item[2].slice(1) };
  const agent = href.match(/^agent:([A-Za-z0-9][A-Za-z0-9_-]{0,63})$/i);
  if (agent) return { kind: "agent", ref: agent[1].toLowerCase() };
  const scheme = href.match(/^(todo|quest|report|practice|team|list|place|region|challenge|course):(.+)$/i);
  if (scheme) {
    const kind = scheme[1].toLowerCase() as "todo" | "quest" | "report" | "practice" | "team" | "list" | "place" | "region" | "challenge" | "course";
    const valid = kind === "practice" ? ATTEMPT_ID : ["team", "list", "place", "region", "challenge", "course"].includes(kind) ? EXACT_ID : ID;
    return valid.test(scheme[2]) ? { kind, ref: scheme[2].toLowerCase() } : null;
  }
  if (href.startsWith("#channel-")) return { kind: "channel", id: href.slice(9) };
  const slack = href.match(SLACK);
  if (slack) return { kind: "slack", url: href, workspace: slack[1], thread: /[?&]thread_ts=/.test(href) };
  const notion = href.match(NOTION);
  if (notion) return { kind: "notion", url: href, workspace: notion[1] ?? notion[2] ?? "", title: titleFromSlug(notion[3]), page: notion[4].toLowerCase(), published: Boolean(notion[1]) };
  const linear = href.match(LINEAR);
  if (linear) return { kind: "linear", url: href, key: linear[1].toUpperCase(), title: titleFromSlug(linear[2]) };
  return null;
}

export function allowedEmbedUrl(url: string): boolean {
  return /^(msg|todo|quest|report|practice|team|list|place|region|challenge|course):/i.test(url) || SETUP.test(url) || url.startsWith("#channel-");
}

const SETUP = /^setup:([a-z][a-z_-]{1,31})$/i;
const CARD_LINE = new RegExp(`^([ \\t]*)((?:msg|todo|quest|report):(?:${REF_SOURCE})(?:${ITEM_SOURCE})?|(?:team|list|place|region|challenge|course):(?:${EXACT_ID_SOURCE})|practice:(?:${ATTEMPT_ID_SOURCE})|setup:[A-Za-z][A-Za-z_-]{1,31}|https?:\\/\\/\\S+)[ \\t]*$`, "gm");
const BARE = new RegExp(`(^|[\\s(])(msg|todo|quest|report):(${REF_SOURCE})(${ITEM_SOURCE})?`, "gi");
const WORLD_BARE = new RegExp(`(^|[\\s(])(team|list|place|region|challenge|course):(${EXACT_ID_SOURCE})(?![0-9a-fA-F-])`, "gi");
const PRACTICE_BARE = new RegExp(`(^|[\\s(])practice:(${ATTEMPT_ID_SOURCE})(?![0-9a-fA-F-])`, "gi");
const CHANNEL = /(^|[\s(])#([A-Za-z][\w-]{0,79})/g;

export function linkEmbeds(text: string, channels: Map<string, string>): string {
  return text
    .replace(CARD_LINE, (whole, lead: string, target: string) => {
      const href = target.replace(/[.,;:!?)]+$/, "");
      return embedFromHref(href) ? `${lead}[](${href} "card")` : whole;
    })
    .replace(BARE, (whole, lead: string, scheme: string, ref: string, item: string | undefined, offset: number, all: string) => {
      if (lead === "(" && all[offset - 1] === "]") return whole;
      const tail = item && scheme.toLowerCase() === "report" ? item : "";
      return `${lead}[${scheme}:${ref}${tail}](${scheme.toLowerCase()}:${ref}${tail})${item && !tail ? item : ""}`;
    })
    .replace(WORLD_BARE, (whole, lead: string, scheme: string, ref: string, offset: number, all: string) =>
      lead === "(" && all[offset - 1] === "]" ? whole : `${lead}[${scheme}:${ref}](${scheme.toLowerCase()}:${ref})`,
    )
    .replace(PRACTICE_BARE, (whole, lead: string, ref: string, offset: number, all: string) => (lead === "(" && all[offset - 1] === "]" ? whole : `${lead}[practice:${ref}](practice:${ref})`))
    .replace(CHANNEL, (whole, lead: string, name: string) => {
      const id = channels.get(name.toLowerCase());
      return id ? `${lead}[#${name}](#channel-${id})` : whole;
    });
}

type Cached<T> = (() => Promise<T>) & { peek: () => T | undefined };

function cached<T>(load: () => Promise<T>, ttl: number, stale?: string): Cached<T> {
  let at = 0;
  let value: Promise<T> | null = null;
  let last: T | undefined;
  if (stale && typeof window !== "undefined") window.addEventListener(stale, () => (value = null));
  const get = () => {
    if (!value || Date.now() - at > ttl) {
      at = Date.now();
      value = load()
        .then((v) => {
          last = v;
          return v;
        })
        .catch(() => {
          value = null;
          return last ?? ([] as unknown as T);
        });
    }
    return value;
  };
  return Object.assign(get, { peek: () => last });
}

const todosOnce = cached<Todo[]>(() => worldApi.todos(), 15_000, "world:todos-stale");
const questsOnce = cached<TaskBrief[]>(() => worldApi.tasks(), 15_000);
const channelsOnce = cached<ChannelSummary[]>(() => worldApi.channels(), 60_000);
const agentsOnce = cached<WorldAgent[]>(() => worldApi.agents(), 60_000);
const slackCache = new Map<string, Promise<SlackDetail | null>>();
const slackSeen = new Map<string, SlackDetail | null>();

type SlackDetail = { author: string | null; channel_name: string | null; team: string | null; kind: string | null; text: string | null; created_at: string };

function useLoaded<T>(load: Cached<T>, key: string): T | undefined {
  const [state, setState] = useState<{ key: string; value: T } | null>(() => {
    const seen = load.peek();
    return seen === undefined ? null : { key, value: seen };
  });
  useEffect(() => {
    let live = true;
    void load().then((value) => live && setState({ key, value }));
    return () => {
      live = false;
    };
  }, [key, load]);
  return state?.key === key ? state.value : undefined;
}

export function useChannelNames(): Map<string, string> {
  const toMap = (list: ChannelSummary[]) => new Map(list.filter((c) => c.kind === "channel").map((c) => [c.name.toLowerCase(), c.id]));
  const [names, setNames] = useState<Map<string, string>>(() => toMap(channelsOnce.peek() ?? []));
  useEffect(() => {
    let live = true;
    void channelsOnce().then((list) => {
      if (!live) return;
      setNames((cur) => {
        const next = toMap(list);
        return next.size === cur.size && [...next].every(([k, v]) => cur.get(k) === v) ? cur : next;
      });
    });
    return () => {
      live = false;
    };
  }, []);
  return names;
}

const CHIP =
  "inline-flex max-w-[min(22rem,80vw)] items-center gap-1 rounded-md border-2 border-w-ink bg-w-sand px-1.5 py-px align-middle text-w-xs font-semibold text-w-warn no-underline shadow-[0_2px_0_rgb(var(--w-ink))] transition-[transform,background-color] duration-100 hover:bg-w-field active:translate-y-px";
const CARD =
  "my-1 flex w-full max-w-lg flex-col gap-1 rounded-lg border-2 border-l-[6px] border-w-ink bg-w-card px-3 py-2 text-left no-underline shadow-[0_2px_0_rgb(var(--w-ink))] transition-[transform,background-color] duration-100 hover:bg-w-field active:translate-y-px";

function Shell({
  card,
  icon,
  label,
  byline,
  body,
  onClick,
  href,
  muted,
}: {
  card: boolean;
  icon: ReactNode;
  label: string;
  byline?: ReactNode;
  body?: ReactNode;
  onClick?: () => void;
  href?: string;
  muted?: boolean;
}) {
  const inner = card ? (
    <>
      <span className="flex min-w-0 items-center gap-1.5 text-w-xs font-semibold text-w-muted">
        <span className="shrink-0">{icon}</span>
        <span className="min-w-0 truncate">{byline ?? label}</span>
      </span>
      {body && <span className={cn("line-clamp-3 text-w-sm leading-snug text-w-text", muted && "text-w-muted line-through")}>{body}</span>}
    </>
  ) : (
    <>
      <span className="shrink-0">{icon}</span>
      <span className={cn("truncate", muted && "line-through opacity-70")}>{label}</span>
    </>
  );
  const cls = card ? CARD : CHIP;
  if (href) {
    return (
      <a href={href} target="_blank" rel="noopener noreferrer" className={cls} onClick={(e) => e.stopPropagation()}>
        {inner}
      </a>
    );
  }
  return (
    <button
      type="button"
      className={cls}
      onClick={(e) => {
        e.stopPropagation();
        onClick?.();
      }}
    >
      {inner}
    </button>
  );
}

function Missing({ label }: { label: string }) {
  return <span className="rounded-md border-2 border-dashed border-w-line2 px-1.5 text-w-xs text-w-muted">{label}</span>;
}

function TodoEmbed({ refId, label, card }: { refId: string; label?: string; card: boolean }) {
  const todos = useLoaded(todosOnce, "todos");
  if (!todos) return <Shell card={card} icon={<Square size={12} weight="bold" />} label={label || "to-do"} />;
  const todo = todos.find((t) => matchesId(t.id, refId));
  if (!todo) return <Missing label={label || "a to-do you cannot open"} />;
  const timer = isTimerTodo(todo);
  const closed = todo.status !== "open";
  const icon = timer ? <Timer size={12} weight="bold" /> : closed ? <CheckSquare size={12} weight="bold" /> : <Square size={12} weight="bold" />;
  const left = todo.nudge_at ? Date.parse(todo.nudge_at) - Date.now() : null;
  const when = timer && left !== null ? (left > 0 ? `goes off in ${countdown(left)}` : "went off") : todo.due_at ? `due ${dayLabel(todo.due_at)} ${messageTime(todo.due_at)}` : null;
  const state = todo.status === "done" ? "done" : todo.status === "dismissed" ? "cancelled" : todo.started_at ? "in progress" : "to do";
  return (
    <Shell
      card={card}
      icon={icon}
      label={label || todo.text}
      muted={closed}
      byline={[timer ? "Timer" : "To-do", state, when].filter(Boolean).join(" · ")}
      body={todo.text}
      onClick={() => focusTodo(todo.id)}
    />
  );
}

function QuestEmbed({ refId, label, card }: { refId: string; label?: string; card: boolean }) {
  const quests = useLoaded(questsOnce, "quests");
  if (!quests) return <Shell card={card} icon={<StatusIcon status="todo" size={12} />} label={label || "quest"} />;
  const quest = quests.find((q) => matchesId(q.id, refId));
  if (!quest) return <Missing label={label || "a quest you cannot open"} />;
  return (
    <Shell
      card={card}
      icon={<StatusIcon status={quest.status} size={12} />}
      label={label || quest.title}
      byline={["Quest", statusLabel(quest.status), quest.repo].filter(Boolean).join(" · ")}
      body={quest.title}
      onClick={() =>
        window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: quest.id, agentId: quest.assigned_agent_id } }))
      }
    />
  );
}

type WorldTarget = Team | WorldItem | RegionDetail;

function WorldTargetEmbed({ kind, refId, label, card }: { kind: "team" | "list" | "place" | "region"; refId: string; label?: string; card: boolean }) {
  const [state, setState] = useState<{ key: string; target: WorldTarget | null } | null>(null);
  const key = `${kind}:${refId}`;
  useEffect(() => {
    const reads = latestOnly();
    const load = () => {
      const isLatest = reads.begin();
      const request = kind === "team" ? teamsApi.get(refId) : kind === "region" ? lokaApi.region(refId) : worldApi.item(refId);
      void request
        .then((target) => {
          const valid =
            kind === "team" ||
            (kind === "region" && "region" in target) ||
            (kind === "list" && "kind" in target && (target.kind === "list" || target.kind === "timer")) ||
            (kind === "place" && "kind" in target && (target.kind === "building" || target.kind === "npc"));
          if (isLatest()) setState({ key, target: valid ? target : null });
        })
        .catch(() => {
          if (isLatest()) setState({ key, target: null });
        });
    };
    const refreshTeam = () => {
      setState(null);
      load();
    };
    const onEvent = (event: Event) => {
      const ev = (event as CustomEvent<WorldEvent>).detail;
      if ((ev.type === "team.updated" || ev.type === "team.deleted") && ev.payload.team_id === refId.toLowerCase()) refreshTeam();
    };
    load();
    if (kind === "team") {
      window.addEventListener("world-event", onEvent);
      window.addEventListener("world-reconnected", refreshTeam);
    }
    return () => {
      reads.begin();
      if (kind === "team") {
        window.removeEventListener("world-event", onEvent);
        window.removeEventListener("world-reconnected", refreshTeam);
      }
    };
  }, [key, kind, refId]);
  const target = state?.key === key ? state.target : undefined;
  const icon =
    kind === "team" ? <UsersThree size={12} weight="bold" /> : kind === "list" ? <ListChecks size={12} weight="bold" /> : kind === "place" ? <Buildings size={12} weight="bold" /> : <MapTrifold size={12} weight="bold" />;
  if (target === undefined) return <Shell card={card} icon={icon} label={label || kind} />;
  if (!target) return <Missing label={label || `a ${kind} you cannot open`} />;
  const name = "region" in target ? target.region.name : target.name;
  const detail =
    kind === "team"
      ? (target as Team).purpose || `${(target as Team).members.length} members`
      : kind === "region"
        ? `${(target as RegionDetail).region.visibility === "public" ? "Shared" : "Private"} ${(target as RegionDetail).region.theme} region`
        : kind === "place"
          ? (target as WorldItem).kind === "npc"
            ? "Character in Loka"
            : "Building in Loka"
          : (target as WorldItem).kind === "timer"
            ? "Timer"
            : `${Array.isArray((target as WorldItem).data.items) ? ((target as WorldItem).data.items as unknown[]).length : 0} items`;
  return <Shell card={card} icon={icon} label={label || name} byline={`${kind[0].toUpperCase()}${kind.slice(1)} · ${detail}`} body={name} onClick={() => openWorldLink(kind, refId)} />;
}

type AcademyRef = { title: string; byline: string; body: string };

function lessonRef(d: ChallengeDetail): AcademyRef {
  const c = d.card;
  const kind = c.type === "canvas" ? "Canvas lesson" : c.type === "reading" ? "Reading lesson" : "Lesson";
  const state = c.state === "passed" ? "passed" : c.attempts > 0 ? `${c.attempts} ${c.attempts === 1 ? "attempt" : "attempts"}` : `${c.xp} xp`;
  return { title: c.title, byline: `${kind} · ${subjectLabel(c.skill)} · ${state}`, body: c.title };
}

function courseRef(a: AdventureCard): AcademyRef {
  const state = a.completed ? "finished" : `${a.passed} of ${a.total} lessons passed`;
  return { title: a.title, byline: `Course${a.skill ? ` · ${subjectLabel(a.skill)}` : ""} · ${state}`, body: a.story ? `${a.title}: ${a.story}` : a.title };
}

const ACADEMY_EVENTS = new Set(["challenge.graded", "challenge.created", "adventure.created", "adventure.completed", "item.updated", "item.removed", "practice.checked", "report.updated"]);

function AcademyEmbed({ kind, refId, label, card }: { kind: "challenge" | "course"; refId: string; label?: string; card: boolean }) {
  const key = `${kind}:${refId}`;
  const [state, setState] = useState<{ key: string; ref: AcademyRef | null } | null>(null);
  useEffect(() => {
    const reads = latestOnly();
    const load = () => {
      const isLatest = reads.begin();
      const request: Promise<AcademyRef | null> =
        kind === "challenge" ? lokaApi.challenge(refId).then(lessonRef) : lokaApi.adventures().then((list) => {
          const found = list.find((a) => a.id === refId);
          return found ? courseRef(found) : null;
        });
      void request
        .then((ref) => {
          if (isLatest()) setState({ key, ref });
        })
        .catch(() => {
          if (isLatest()) setState({ key, ref: null });
        });
    };
    const onEvent = (event: Event) => {
      const ev = (event as CustomEvent<WorldEvent>).detail;
      if (ev && ACADEMY_EVENTS.has(ev.type)) load();
    };
    load();
    window.addEventListener("world-event", onEvent);
    window.addEventListener("world-reconnected", load);
    return () => {
      reads.begin();
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener("world-reconnected", load);
    };
  }, [key, kind, refId]);
  const icon = kind === "course" ? <Books size={12} weight="bold" /> : <Student size={12} weight="bold" />;
  const ref = state?.key === key ? state.ref : undefined;
  if (ref === undefined) return <Shell card={card} icon={icon} label={label || (kind === "course" ? "course" : "lesson")} />;
  if (!ref) return <Missing label={label || `a ${kind === "course" ? "course" : "lesson"} you cannot open`} />;
  return <Shell card={card} icon={icon} label={label || ref.title} byline={ref.byline} body={ref.body} onClick={() => openAcademy(kind === "course" ? { courseId: refId } : { challengeId: refId })} />;
}

function AgentEmbed({ refId, label, card }: { refId: string; label?: string; card: boolean }) {
  const agents = useLoaded(agentsOnce, "agents");
  const icon = <UserCircle size={12} weight="bold" />;
  if (!agents) return <Shell card={card} icon={icon} label={label || "agent"} />;
  const agent = agents.find((a) => a.slug.toLowerCase() === refId || matchesId(a.id, refId));
  if (!agent) return <Missing label={label || "an agent you cannot open"} />;
  return (
    <Shell
      card={card}
      icon={<AgentFace agent={agent} size={14} />}
      label={label || agent.name}
      byline={["Agent", `@${agent.slug}`, agent.is_main ? "main" : null].filter(Boolean).join(" · ")}
      body={
        <>
          <span className="block font-semibold">{agent.name}</span>
          {agent.expertise && <span className="block text-w-text2">{agent.expertise}</span>}
        </>
      }
      onClick={() => window.dispatchEvent(new CustomEvent("world:open-agent", { detail: { id: agent.id } }))}
    />
  );
}

const reportCache = new Map<string, Promise<ReportBrief | null>>();
const reportSeen = new Map<string, ReportBrief | null>();
if (typeof window !== "undefined")
  window.addEventListener(REPORTS_STALE, () => {
    reportCache.clear();
  });

function reportBrief(ref: string): Promise<ReportBrief | null> {
  const hit = reportCache.get(ref);
  if (hit) return hit;
  const pending = reportsApi
    .find(ref)
    .catch(() => null)
    .then((r) => {
      reportSeen.set(ref, r);
      return r;
    });
  reportCache.set(ref, pending);
  return pending;
}

function plainExcerpt(text: string): string {
  return text
    .replace(/```[\s\S]*?(```|$)/g, " ")
    .replace(/^#+\s.*$/gm, " ")
    .replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_, t: string, a?: string) => a ?? t)
    .replace(/[*`>|#]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

const itemCache = new Map<string, Promise<CanvasItem | null>>();
if (typeof window !== "undefined") window.addEventListener(REPORTS_STALE, () => itemCache.clear());

function canvasItem(report: string, item: string): Promise<CanvasItem | null> {
  const key = `${report}#${item}`;
  const hit = itemCache.get(key);
  if (hit) return hit;
  const pending = reportsApi
    .get(report)
    .then((d) => d.report.canvas?.items.find((i) => i.id === item) ?? null)
    .catch(() => null);
  itemCache.set(key, pending);
  return pending;
}

function CanvasItemEmbed({ report, itemId, label, card }: { report: ReportBrief; itemId: string; label?: string; card: boolean }) {
  const [state, setState] = useState<{ key: string; item: CanvasItem | null } | null>(null);
  const key = `${report.id}#${itemId}`;
  useEffect(() => {
    let live = true;
    void canvasItem(report.id, itemId).then((item) => live && setState({ key, item }));
    return () => {
      live = false;
    };
  }, [report.id, itemId, key]);
  const item = state?.key === key ? state.item : undefined;
  const target = item?.kind === "ref" ? embedFromHref(item.target) : null;
  if (target && card)
    return (
      <span className="my-1 flex w-full max-w-lg flex-col">
        <EmbedView embed={target} card />
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            openReport(report.id, itemId);
          }}
          className="inline-flex items-center gap-1 self-start rounded-md px-1 text-w-xs font-semibold text-w-link hover:bg-w-sand"
        >
          <FlowArrow size={11} weight="bold" />
          On canvas {report.title}
        </button>
      </span>
    );
  const what = item ? canvasItemKind(item) : "Item";
  const text = item ? canvasItemLabel(item) || what : item === null ? "This item is no longer on the canvas" : "";
  return (
    <Shell
      card={card}
      icon={<FlowArrow size={12} weight="bold" />}
      label={label || `${what} on ${report.title}`}
      muted={item === null}
      byline={[`${what} on canvas`, report.title].join(" · ")}
      body={text || undefined}
      onClick={() => openReport(report.id, itemId)}
    />
  );
}

function ReportEmbed({ refId, item, label, card }: { refId: string; item?: string; label?: string; card: boolean }) {
  const [state, setState] = useState<{ ref: string; report: ReportBrief | null } | null>(() => (reportSeen.has(refId) ? { ref: refId, report: reportSeen.get(refId) ?? null } : null));
  useEffect(() => {
    let live = true;
    void reportBrief(refId).then((report) => live && setState({ ref: refId, report }));
    return () => {
      live = false;
    };
  }, [refId]);
  const icon = <FileText size={12} weight="bold" />;
  if (!state || state.ref !== refId) return <Shell card={card} icon={icon} label={label || "report"} />;
  const report = state.report;
  if (!report) return <Missing label={label || "a report you cannot open"} />;
  const canvas = report.kind === "canvas";
  if (report.practice) return <PracticeReportEmbed report={report} label={label} card={card} />;
  if (canvas && item) return <CanvasItemEmbed report={report} itemId={item} label={label} card={card} />;
  const what = canvas
    ? [`Canvas · ${report.item_count} ${report.item_count === 1 ? "item" : "items"}`, report.open_marks ? `${report.open_marks} open ${report.open_marks === 1 ? "mark" : "marks"}` : null]
    : ["Report"];
  return (
    <Shell
      card={card}
      icon={canvas ? <FlowArrow size={12} weight="bold" /> : icon}
      label={label || report.title}
      byline={[...what, report.folder || null, `${dayLabel(report.updated_at)} ${messageTime(report.updated_at)}`].filter(Boolean).join(" · ")}
      body={
        <>
          <span className="block font-semibold">{report.title}</span>
          <span className="block text-w-text2">{plainExcerpt(report.excerpt)}</span>
        </>
      }
      onClick={() => openReport(report.id)}
    />
  );
}

function PracticeReportEmbed({ report, label, card }: { report: ReportBrief; label?: string; card: boolean }) {
  const agents = useLoaded(agentsOnce, "agents");
  const practice = report.practice;
  const coach = practice?.coach_agent_id ? agents?.find((a) => a.id === practice.coach_agent_id) : undefined;
  const subject = practice ? subjectLabel(practice.subject) : "";
  return (
    <Shell
      card={card}
      icon={<Student size={12} weight="bold" />}
      label={label || report.title}
      byline={["Practice", subject, coach ? `coach ${coach.name}` : null, `${dayLabel(report.updated_at)} ${messageTime(report.updated_at)}`].filter(Boolean).join(" · ")}
      body={
        <>
          <span className="block font-semibold">{report.title}</span>
          <span className="flex items-center gap-1.5 text-w-text2">
            {coach && <AgentFace agent={coach} size={16} />}
            {coach ? `${coach.name} checks each attempt` : "Pick a coach to get each attempt checked"}
          </span>
        </>
      }
      onClick={() => openPractice(report.id)}
    />
  );
}

const attemptCache = new Map<string, Promise<AttemptCard | null>>();
const attemptSeen = new Map<string, AttemptCard | null>();
if (typeof window !== "undefined")
  window.addEventListener("world-event", (e) => {
    const type = (e as CustomEvent<{ type?: string; payload?: { attempt_id?: unknown } }>).detail?.type;
    if (type === "practice.checked" || type === "practice.ready") attemptCache.clear();
  });

function attemptCard(ref: string): Promise<AttemptCard | null> {
  const hit = attemptCache.get(ref);
  if (hit) return hit;
  const pending = practiceApi
    .attempt(ref)
    .catch(() => null)
    .then((a) => {
      attemptSeen.set(ref, a);
      return a;
    });
  attemptCache.set(ref, pending);
  return pending;
}

function PracticeEmbed({ refId, label, card }: { refId: string; label?: string; card: boolean }) {
  const [state, setState] = useState<{ ref: string; attempt: AttemptCard | null } | null>(() => (attemptSeen.has(refId) ? { ref: refId, attempt: attemptSeen.get(refId) ?? null } : null));
  const [stamp, setStamp] = useState(0);
  useEffect(() => {
    const on = (e: Event) => {
      const ev = (e as CustomEvent<{ type?: string; payload?: { attempt_id?: unknown } }>).detail;
      if (ev?.type?.startsWith("practice.") && typeof ev.payload?.attempt_id === "string" && ev.payload.attempt_id.startsWith(refId)) setStamp(Date.now());
    };
    window.addEventListener("world-event", on);
    return () => window.removeEventListener("world-event", on);
  }, [refId]);
  useEffect(() => {
    let live = true;
    void attemptCard(refId).then((attempt) => live && setState({ ref: refId, attempt }));
    return () => {
      live = false;
    };
  }, [refId, stamp]);
  const icon = <Student size={12} weight="bold" />;
  if (!state || state.ref !== refId) return <Shell card={card} icon={icon} label={label || "practice attempt"} />;
  const attempt = state.attempt;
  if (!attempt) return <Missing label="Attempt not found" />;
  const check = checkOf(attempt.check);
  const open = () => openPractice(attempt.report_id, attempt.id);
  if (!card) return <Shell card={false} icon={icon} label={label || `Attempt ${attempt.n} of ${attempt.title}`} onClick={open} />;
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        open();
      }}
      className={cn(CARD, "!flex-row items-start gap-3")}
    >
      {attempt.image_upload_id ? (
        <img src={`/api/world/uploads/${attempt.image_upload_id}`} alt="" loading="lazy" className="h-16 w-16 shrink-0 rounded-md border-2 border-w-line2 bg-w-field object-contain" />
      ) : (
        <span className="grid h-16 w-16 shrink-0 place-items-center rounded-md border-2 border-dashed border-w-line2 text-w-muted">{icon}</span>
      )}
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-center gap-1.5 text-w-xs font-semibold text-w-muted">
          {icon}
          <span className="min-w-0 truncate">{[`Attempt ${attempt.n}`, subjectLabel(attempt.subject), check ? (typeof check.score === "number" ? `${check.score}/100` : "checked") : "waiting for the coach"].join(" · ")}</span>
        </span>
        <span className="truncate text-w-sm font-semibold text-w-text">{label || attempt.title}</span>
        {check ? (
          <span className="line-clamp-2 text-w-sm leading-snug text-w-text2">Fix: {check.fix}</span>
        ) : (
          <span className="text-w-sm text-w-muted">Not checked yet</span>
        )}
      </span>
    </button>
  );
}

function ChannelEmbed({ id, label }: { id: string; label?: string }) {
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        window.dispatchEvent(new CustomEvent("world:open-channel", { detail: { id } }));
      }}
      className="inline-flex items-center rounded-md bg-w-teal/20 px-0.5 font-medium text-w-link hover:bg-w-teal/35"
    >
      <Hash size={12} weight="bold" className="shrink-0" />
      {(label ?? "").replace(/^#/, "") || "channel"}
    </button>
  );
}

function slackDetail(url: string): Promise<SlackDetail | null> {
  const hit = slackCache.get(url);
  if (hit) return hit;
  const pending = fetch(`/api/world/embeds/slack?url=${encodeURIComponent(url)}`)
    .then((r) => (r.ok ? (r.json() as Promise<SlackDetail>) : null))
    .catch(() => null)
    .then((d) => {
      slackSeen.set(url, d);
      return d;
    });
  slackCache.set(url, pending);
  return pending;
}

function SlackEmbed({ url, workspace, thread, label, card }: { url: string; workspace: string; thread: boolean; label?: string; card: boolean }) {
  const [detail, setDetail] = useState<SlackDetail | null>(() => slackSeen.get(url) ?? null);
  useEffect(() => {
    let live = true;
    void slackDetail(url).then((d) => live && setDetail(d));
    return () => {
      live = false;
    };
  }, [url]);
  const where = detail?.channel_name ? `#${detail.channel_name}` : thread ? "thread" : "message";
  const team = detail?.team || workspace;
  const fallback = `${team} · ${where}`;
  const chip = label || (detail?.author ? `${detail.author} in ${where}${detail.text ? `: ${detail.text}` : ""}` : `Slack ${fallback}`);
  return (
    <Shell
      card={card}
      href={url}
      icon={<SlackMark size={12} />}
      label={chip}
      byline={
        <>
          {detail?.author ? `${detail.author} in ${where}` : `Slack · ${fallback}`}
          {detail && <span className="ml-1 font-normal normal-case tracking-normal">{`${dayLabel(detail.created_at)}, ${messageTime(detail.created_at)}`}</span>}
        </>
      }
      body={detail?.text || (card ? "Open in Slack" : undefined)}
    />
  );
}

export function LinearMark({ size = 12 }: { size?: number }) {
  return (
    <svg viewBox="0 0 100 100" width={size} height={size} aria-hidden>
      <circle cx="50" cy="50" r="50" fill="#5E6AD2" />
      <path d="M18 58 42 82M14 44l42 42M18 30l52 52M28 20l52 52M42 14l44 44" stroke="#fff" strokeWidth="7" strokeLinecap="round" />
    </svg>
  );
}

export function NotionMark({ size = 12 }: { size?: number }) {
  return (
    <svg viewBox="0 0 100 100" width={size} height={size} aria-hidden>
      <rect x="6" y="6" width="88" height="88" rx="14" fill="#fff" stroke="#111" strokeWidth="8" />
      <path d="M32 72V28h6l24 34V28h6v44h-6L38 38v34z" fill="#111" />
    </svg>
  );
}

function NotionEmbed({ url, title, workspace, page, published, label, card }: { url: string; title: string; workspace: string; page: string; published: boolean; label?: string; card: boolean }) {
  const [open, setOpen] = useState(false);
  const shell = (
    <Shell
      card={card}
      href={url}
      icon={<NotionMark size={12} />}
      label={label || title || "Notion page"}
      byline={
        <>
          Notion{workspace ? ` · ${workspace}` : ""}
          <ArrowSquareOut size={11} weight="bold" className="ml-1 inline align-[-1px]" />
        </>
      }
      body={title || "Open in Notion"}
    />
  );
  if (!card || !published) return shell;
  return (
    <span className="my-1 flex w-full max-w-lg flex-col">
      {shell}
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
        className="mt-1 self-start rounded-md border-2 border-w-ink bg-w-sand px-2 py-0.5 text-w-xs font-semibold text-w-text shadow-[0_2px_0_rgb(var(--w-ink))] hover:bg-w-field active:translate-y-px"
      >
        {open ? "Hide preview" : "Preview page"}
      </button>
      {open && (
        <iframe
          title={title || "Notion page"}
          src={`https://${workspace}.notion.site/ebd/${page}`}
          loading="lazy"
          referrerPolicy="no-referrer"
          sandbox="allow-scripts allow-same-origin allow-popups allow-popups-to-escape-sandbox"
          className="mt-1 h-[28rem] w-full rounded-lg border-2 border-w-ink bg-w-card"
          {...{ credentialless: "" }}
        />
      )}
    </span>
  );
}

function LinearEmbed({ url, issue, title, label, card }: { url: string; issue: string; title: string; label?: string; card: boolean }) {
  return (
    <Shell
      card={card}
      href={url}
      icon={<LinearMark size={12} />}
      label={label || (title ? `${issue} ${title}` : issue)}
      byline={
        <>
          Linear · {issue}
          <ArrowSquareOut size={11} weight="bold" className="ml-1 inline align-[-1px]" />
        </>
      }
      body={title || "Open in Linear"}
    />
  );
}

export function EmbedView({ embed, label, card }: { embed: Embed; label?: string; card: boolean }) {
  switch (embed.kind) {
    case "msg":
      return <MessageLink reference={embed.ref} label={label} card={card} />;
    case "todo":
      return <TodoEmbed refId={embed.ref} label={label} card={card} />;
    case "quest":
      return <QuestEmbed refId={embed.ref} label={label} card={card} />;
    case "agent":
      return <AgentEmbed refId={embed.ref} label={label} card={card} />;
    case "report":
      return <ReportEmbed refId={embed.ref} item={embed.item} label={label} card={card} />;
    case "practice":
      return <PracticeEmbed refId={embed.ref} label={label} card={card} />;
    case "team":
    case "list":
    case "place":
    case "region":
      return <WorldTargetEmbed kind={embed.kind} refId={embed.ref} label={label} card={card} />;
    case "challenge":
    case "course":
      return <AcademyEmbed kind={embed.kind} refId={embed.ref} label={label} card={card} />;
    case "setup":
      return <SetupCard featureKey={embed.key} />;
    case "channel":
      return <ChannelEmbed id={embed.id} label={label} />;
    case "slack":
      return <SlackEmbed url={embed.url} workspace={embed.workspace} thread={embed.thread} label={label} card={card} />;
    case "notion":
      return <NotionEmbed url={embed.url} title={embed.title} workspace={embed.workspace} page={embed.page} published={embed.published} label={label} card={card} />;
    case "linear":
      return <LinearEmbed url={embed.url} issue={embed.key} title={embed.title} label={label} card={card} />;
  }
}
