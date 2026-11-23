"use client";

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ArrowBendUpLeft, Check, LinkSimple } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { dayLabel, messageTime } from "@/lib/world";
import type { RoomMessageKind } from "@/lib/quest-board";
import { scrollWithin } from "@/lib/scroll";
import { ATTEMPT_ID } from "@/lib/practice";
import { validFeatureItem } from "@/lib/feature-links";

export type LinkedMessage = {
  ref: string;
  kind: "agent" | "channel";
  agent_id: string | null;
  channel_id: string | null;
  channel_name: string | null;
  channel_kind: string | null;
  author: string;
  author_kind: string;
  author_emoji: string | null;
  excerpt: string;
  created_at: string;
  task_id?: string | null;
  room_kind?: RoomMessageKind | null;
};

const REF_SOURCE = "ch\\d{1,18}|[0-9a-fA-F][0-9a-fA-F-]{5,35}";
const REF = new RegExp(`^(?:${REF_SOURCE})$`);

export function messageRefFromHref(href?: string | null): string | null {
  if (!href) return null;
  if (/^msg:/i.test(href)) {
    const ref = href.slice(4);
    return REF.test(ref) ? ref : null;
  }
  if (typeof window === "undefined") return null;
  try {
    const url = new URL(href, window.location.origin);
    if (url.pathname.replace(/\/$/, "") !== "/world") return null;
    const ref = url.searchParams.get("msg");
    return ref && REF.test(ref) ? ref : null;
  } catch {
    return null;
  }
}

export function messageUrl(ref: string): string {
  return `${window.location.origin}/world?msg=${ref}`;
}

export type WorldLinkKind = "quest" | "report" | "todo" | "practice" | "team" | "list" | "place" | "region";
const WORLD_LINK_KINDS: WorldLinkKind[] = ["quest", "report", "todo", "practice", "team", "list", "place", "region"];
const EXACT_WORLD_LINK_KINDS = new Set<WorldLinkKind>(["team", "list", "place", "region"]);
const WORLD_ID = /^[0-9a-fA-F][0-9a-fA-F-]{5,35}$/;
const EXACT_WORLD_ID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

export function worldUrl(kind: WorldLinkKind, id: string): string {
  return `${window.location.origin}/world?${kind}=${id}`;
}

export function worldRefFromHref(href?: string | null): { kind: WorldLinkKind; ref: string; item?: string } | null {
  if (!href || typeof window === "undefined" || !/^https?:/i.test(href)) return null;
  try {
    const url = new URL(href);
    if (url.pathname.replace(/\/$/, "") !== "/world") return null;
    for (const kind of WORLD_LINK_KINDS) {
      const ref = url.searchParams.get(kind);
      if (!ref || !(kind === "practice" ? ATTEMPT_ID : EXACT_WORLD_LINK_KINDS.has(kind) ? EXACT_WORLD_ID : WORLD_ID).test(ref)) continue;
      if (EXACT_WORLD_LINK_KINDS.has(kind) && url.origin !== window.location.origin) continue;
      const item = kind === "report" ? url.searchParams.get("item") : null;
      return validFeatureItem(item) ? { kind, ref: ref.toLowerCase(), item } : { kind, ref: ref.toLowerCase() };
    }
  } catch {
    return null;
  }
  return null;
}

export function openWorldLink(kind: Extract<WorldLinkKind, "team" | "list" | "place" | "region">, id: string): void {
  window.dispatchEvent(new CustomEvent("world:open-link", { detail: { kind, id } }));
}

export function channelRef(id: number): string {
  return `ch${id}`;
}

const cache = new Map<string, Promise<LinkedMessage | null>>();
const resolved = new Map<string, LinkedMessage>();

export function resolveMessage(ref: string, fresh = false): Promise<LinkedMessage | null> {
  const key = ref.toLowerCase();
  const hit = cache.get(key);
  if (hit && !fresh) return hit;
  const pending = fetch(`/api/world/messages/${encodeURIComponent(ref)}`)
    .then((r) => (r.ok ? (r.json() as Promise<LinkedMessage>) : null))
    .catch(() => null)
    .then((found) => {
      if (!found) window.setTimeout(() => cache.delete(key), 30_000);
      else resolved.set(key, found);
      return found;
    });
  if (!fresh) cache.set(key, pending);
  return pending;
}

function useLinkedMessage(ref: string): LinkedMessage | null | undefined {
  const [state, setState] = useState<{ ref: string; link: LinkedMessage | null } | null>(() => {
    const seen = resolved.get(ref.toLowerCase());
    return seen ? { ref, link: seen } : null;
  });
  useEffect(() => {
    let live = true;
    void resolveMessage(ref).then((link) => live && setState({ ref, link }));
    return () => {
      live = false;
    };
  }, [ref]);
  return state?.ref === ref ? state.link : undefined;
}

export function openMessage(link: LinkedMessage) {
  window.dispatchEvent(new CustomEvent<LinkedMessage>("world:open-message", { detail: link }));
}

type Jump = { id: string; agentId: string | null; channelId: string | null; current: () => boolean };
let pendingJump: Jump | null = null;

export function requestJump(link: LinkedMessage, current: () => boolean = () => true) {
  pendingJump = { id: link.ref, agentId: link.agent_id, channelId: link.channel_id, current };
  window.dispatchEvent(new Event("world:message-jump"));
}

const MAX_PAGES = 15;

export function useMessageJump(
  scope: { agentId?: string | null; channelId?: string | null },
  ids: string[],
  loadOlder: () => Promise<boolean>,
  onFound?: (id: string) => void,
): string | null {
  const [tick, setTick] = useState(0);
  const [highlight, setHighlight] = useState<string | null>(null);
  const pages = useRef(0);
  const loading = useRef(false);
  const timer = useRef<number | null>(null);
  const foundRef = useRef(onFound);
  foundRef.current = onFound;
  useEffect(() => {
    const bump = () => setTick((t) => t + 1);
    window.addEventListener("world:message-jump", bump);
    return () => window.removeEventListener("world:message-jump", bump);
  }, []);
  useEffect(() => () => {
    if (timer.current) window.clearTimeout(timer.current);
  }, []);
  const key = ids.join(",");
  useEffect(() => {
    const target = pendingJump;
    if (!target) return;
    if (!target.current()) {
      if (pendingJump === target) pendingJump = null;
      return;
    }
    const mine = scope.agentId ? target.agentId === scope.agentId : !!scope.channelId && target.channelId === scope.channelId;
    if (!mine) return;
    if (ids.includes(target.id)) {
      pendingJump = null;
      pages.current = 0;
      foundRef.current?.(target.id);
      window.setTimeout(() => {
        if (!target.current() || pendingJump && pendingJump !== target) return;
        const el = document.querySelector(`[data-message-id="${CSS.escape(target.id)}"]`);
        if (!el) return;
        const far = Math.abs(el.getBoundingClientRect().top - window.innerHeight / 2) > window.innerHeight * 1.5;
        scrollWithin(el, { block: "center", smooth: !far });
        setHighlight(target.id);
        if (timer.current) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setHighlight((h) => (h === target.id ? null : h)), 2600);
      }, 60);
      return;
    }
    if (!ids.length || loading.current) return;
    if (pages.current >= MAX_PAGES) {
      pendingJump = null;
      pages.current = 0;
      return;
    }
    loading.current = true;
    pages.current += 1;
    void loadOlder()
      .catch(() => false)
      .then((more) => {
        loading.current = false;
        if (!target.current() || pendingJump !== target) return;
        if (!more) {
          pendingJump = null;
          pages.current = 0;
        } else {
          setTick((t) => t + 1);
        }
      });
  }, [tick, key, scope.agentId, scope.channelId, loadOlder]);
  return highlight;
}

export const HIGHLIGHT = "ring-4 ring-w-gold ring-offset-2 ring-offset-w-paper transition-shadow duration-300";

function placeOf(link: LinkedMessage): string {
  if (link.kind === "channel") return link.channel_kind === "dm" ? "in a direct message" : `in #${link.channel_name ?? "a channel"}`;
  return "";
}

function when(at: string): string {
  return `${dayLabel(at)}, ${messageTime(at)}`;
}

function Byline({ link }: { link: LinkedMessage }) {
  return (
    <span className="flex min-w-0 items-center gap-1 text-w-xs font-semibold text-w-muted">
      <ArrowBendUpLeft size={12} weight="bold" className="shrink-0" />
      <span className="truncate">
        {link.author_emoji ? `${link.author_emoji} ` : ""}
        {link.author} {placeOf(link)}
      </span>
      <span className="shrink-0 font-normal normal-case tracking-normal">{when(link.created_at)}</span>
    </span>
  );
}

export function MessageLink({ reference, label, card = false }: { reference: string; label?: string; card?: boolean }) {
  const link = useLinkedMessage(reference);
  const open = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (link) openMessage(link);
  };
  if (link === null) {
    return (
      <span title="This message is gone, or you cannot open it" className="rounded-md border-2 border-dashed border-w-line2 px-1.5 text-w-xs text-w-muted">
        {label || "a message you cannot open"}
      </span>
    );
  }
  if (card) {
    return (
      <button
        type="button"
        onClick={open}
        disabled={!link}
        aria-label={link ? `Open ${link.author}'s message` : "Loading message"}
        className="my-1 flex w-full max-w-lg flex-col gap-1 rounded-lg border-2 border-l-[6px] border-w-ink bg-w-card px-3 py-2 text-left shadow-[0_2px_0_rgb(var(--w-ink))] transition-[transform,background-color] duration-100 hover:bg-w-field active:translate-y-px"
      >
        {link ? (
          <>
            <Byline link={link} />
            <span className="line-clamp-3 text-w-sm leading-snug text-w-text">{link.excerpt || "(no text)"}</span>
          </>
        ) : (
          <span className="h-9 w-full animate-[fade-in_200ms_ease-out] rounded-md bg-w-sand/70" />
        )}
      </button>
    );
  }
  const text = label || (link ? `${link.author}: ${link.excerpt}` : "message");
  return (
    <span className="group/link relative inline-block max-w-full align-bottom">
      <button
        type="button"
        onClick={open}
        disabled={!link}
        className="inline-flex max-w-[min(22rem,80vw)] items-center gap-1 rounded-md border-2 border-w-ink bg-w-sand px-1.5 py-px align-middle text-w-xs font-semibold text-w-warn shadow-[0_2px_0_rgb(var(--w-ink))] transition-[transform,background-color] duration-100 hover:bg-w-field active:translate-y-px"
      >
        <ArrowBendUpLeft size={12} weight="bold" className="shrink-0" />
        <span className="truncate">{text}</span>
      </button>
      {link && (
        <span
          role="tooltip"
          className="pointer-events-none invisible absolute bottom-full left-0 z-30 mb-1.5 flex w-72 max-w-[80vw] flex-col gap-1 rounded-lg border-2 border-w-ink bg-w-card p-2 text-left font-normal opacity-0 shadow-[0_3px_0_rgb(var(--w-ink))] transition-opacity duration-150 group-focus-within/link:visible group-focus-within/link:opacity-100 group-hover/link:visible group-hover/link:opacity-100 [@media(hover:none)]:hidden"
        >
          <Byline link={link} />
          <span className="line-clamp-4 text-w-sm leading-snug text-w-text">{link.excerpt || "(no text)"}</span>
        </span>
      )}
    </span>
  );
}

export function textOf(children: ReactNode): string {
  if (typeof children === "string" || typeof children === "number") return String(children);
  if (Array.isArray(children)) return children.map(textOf).join("");
  return "";
}

export function LinkButton({ reference, className }: { reference: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const id = window.setTimeout(() => setCopied(false), 1400);
    return () => window.clearTimeout(id);
  }, [copied]);
  const copy = useCallback(
    async (e: React.MouseEvent) => {
      e.stopPropagation();
      try {
        await navigator.clipboard.writeText(messageUrl(reference));
        setCopied(true);
      } catch {
        setCopied(false);
      }
    },
    [reference],
  );
  return (
    <button
      type="button"
      onClick={(e) => void copy(e)}
      aria-label={copied ? "Link copied" : "Copy link to message"}
      title={copied ? "Link copied" : "Copy link to message"}
      className={cn(
        "rounded-md p-0.5 text-w-muted transition-opacity hover:bg-w-sand hover:text-w-strong focus-visible:opacity-100 group-hover:opacity-100 [@media(hover:none)]:opacity-100",
        copied ? "text-w-ok opacity-100" : "opacity-0",
        className,
      )}
    >
      {copied ? <Check size={12} weight="bold" /> : <LinkSimple size={12} weight="bold" />}
    </button>
  );
}

export function WorldLinkButton({ kind, id, label, className }: { kind: WorldLinkKind; id: string; label: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timeout = window.setTimeout(() => setCopied(false), 1400);
    return () => window.clearTimeout(timeout);
  }, [copied]);
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        void navigator.clipboard.writeText(worldUrl(kind, id)).then(() => setCopied(true)).catch(() => setCopied(false));
      }}
      aria-label={copied ? "Link copied" : `Copy link to ${label}`}
      title={copied ? "Link copied" : "Copy link"}
      className={cn("rounded-md p-0.5 text-w-muted hover:bg-w-sand hover:text-w-strong", copied && "text-w-ok", className)}
    >
      {copied ? <Check size={12} weight="bold" /> : <LinkSimple size={12} weight="bold" />}
    </button>
  );
}
