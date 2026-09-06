"use client";

import { useEffect, useMemo } from "react";
import { Bell, BellRinging, BookOpenText, GearSix, ChatsTeardrop, ClockCounterClockwise, CrownSimple, Hammer, IdentificationBadge, Lightning, Microphone, Notepad, Scroll, TerminalWindow, type Icon } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { Progress } from "@/lib/loka";
import { portraitUrl } from "./characters";
import { AgentAvatar } from "./channel-ui";
import { PLAYER_LOOK } from "./engine";
import { BADGE, FOCUS, HUD_ICON, PIXEL } from "./styles";
import { GlyphTile, eventGlyph } from "./event-glyphs";
import Glide from "./glide";

export type Role = "view" | "read" | "write" | "admin";

export interface Me {
  id: string;
  name: string | null;
  email: string | null;
  handle: string;
  role: Role;
  main_agent_id: string | null;
  status_emoji?: string | null;
  status_text?: string | null;
  mention_wakes_main?: boolean;
  onboarding?: boolean;
}

const RANK: Record<Role, number> = { view: 0, read: 1, write: 2, admin: 3 };

export function atLeast(me: Me | null, role: Role): boolean {
  return RANK[me?.role ?? "view"] >= RANK[role];
}

export function PlayerCard({ me, progress, onOpen, onProgress }: { me: Me | null; progress: Progress | null; onOpen: () => void; onProgress: () => void }) {
  const portrait = useMemo(() => portraitUrl(PLAYER_LOOK, 4), []);
  const name = me?.name || me?.email?.split("@")[0] || "You";
  const status = Boolean(me?.status_emoji || me?.status_text);
  return (
    <div className="pointer-events-auto flex w-[15.5rem] flex-col items-stretch gap-1.5">
    <div className="overflow-hidden rounded-xl border-[3px] border-w-ink bg-w-paper/95 shadow-[0_4px_0_rgb(var(--w-ink))]">
      <button
        type="button"
        onClick={onOpen}
        title="Profile and settings (Y)"
        aria-label={`${name}, open profile and settings`}
        className={cn("group flex w-full items-center gap-3 p-2 pr-3 text-left transition-colors duration-200 hover:bg-w-sand/70", FOCUS, "focus-visible:ring-inset focus-visible:ring-offset-0")}
      >
        <AgentAvatar portrait={portrait} size="3.5rem" tall />
        <span className="min-w-0 flex-1">
          <span className={cn(PIXEL, "block truncate text-w-lg leading-none text-w-text")}>{name}</span>
          <span className="mt-1.5 block truncate text-w-xs text-w-muted">
            {status ? (
              <>
                {me?.status_emoji ? <span className="mr-1">{me.status_emoji}</span> : null}
                {me?.status_text}
              </>
            ) : me?.handle ? `@${me.handle}` : "Set your name"}
          </span>
        </span>
        <GearSix
          size={16}
          weight="bold"
          aria-hidden
          className="shrink-0 -translate-x-1 text-w-muted opacity-0 transition-[opacity,transform] duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] group-hover:translate-x-0 group-hover:opacity-100 group-focus-visible:translate-x-0 group-focus-visible:opacity-100 motion-reduce:transform-none"
        />
      </button>
      {progress && <XpRow progress={progress} onClick={onProgress} />}
    </div>
    </div>
  );
}

function XpRow({ progress, onClick }: { progress: Progress; onClick: () => void }) {
  const span = Math.max(1, progress.next_level_xp - progress.level_xp);
  const into = Math.max(0, Math.min(span, progress.xp - progress.level_xp));
  const pct = Math.round((into / span) * 100);
  return (
    <button
      type="button"
      onClick={onClick}
      title={`${progress.next_level_xp - progress.xp} xp to level ${progress.level + 1}`}
      className={cn("flex w-full items-center gap-2 border-t-2 border-dashed border-w-line px-3 py-1.5 text-left transition-colors duration-200 hover:bg-w-sand/70", FOCUS, "focus-visible:ring-inset focus-visible:ring-offset-0")}
    >
      <span className={cn(PIXEL, "shrink-0 text-w-sm leading-none text-w-text")}>Lv {progress.level}</span>
      <span className="h-2.5 min-w-[4rem] flex-1 overflow-hidden rounded-[0.1875rem] border-[1.5px] border-w-ink bg-w-field p-px" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label="Progress to the next level">
        <span className="block h-full rounded-[1px] bg-w-teal bg-[repeating-linear-gradient(90deg,transparent_0_5px,rgb(var(--w-ink)/0.18)_5px_6px)] transition-[width] duration-500 motion-reduce:transition-none" style={{ width: `${pct}%` }} />
      </span>
      <span className="shrink-0 font-mono text-w-2xs tabular-nums text-w-muted">{progress.xp} xp</span>
      {progress.streak_alive && progress.streak > 1 && <span className="shrink-0 font-mono text-w-2xs text-w-warn" title={`${progress.streak} day streak`}>{progress.streak}d</span>}
    </button>
  );
}

interface Slot {
  key: string;
  label: string;
  hint?: string;
  icon: Icon;
  image?: string;
  group?: number;
  badge?: number;
  tone?: "gold" | "teal" | "plain" | "live";
  onClick?: () => void;
  onPress?: () => void;
  onRelease?: () => void;
}

const SLOT_TONE = {
  plain: "hotbar-glide text-w-text hover:text-w-strong",
  teal: "bg-w-seafoam/70 text-w-strong hover:bg-w-seafoam",
  gold: "bg-w-crown text-w-ink hover:brightness-105",
  live: "bg-w-coral text-w-ink",
} as const;

export function Hotbar({ slots }: { slots: Slot[] }) {
  return (
    <nav
      aria-label="Hotbar"
      className="pointer-events-auto relative flex max-w-full items-center gap-0.5 overflow-x-auto overscroll-x-contain rounded-2xl border-2 border-w-ink bg-w-paper/90 px-1.5 pb-1.5 pt-2.5 shadow-[0_4px_0_rgb(var(--w-ink)),0_18px_40px_-16px_rgb(0_0_0/0.55)] backdrop-blur-md [scrollbar-width:none] sm:overflow-visible sm:pt-1.5"
    >
      <Glide hover=".hotbar-glide" className="rounded-xl bg-w-sand" />
      {slots.map((s, i) => (
        <div key={s.key} className="relative z-[1] flex shrink-0 items-center">
          {i > 0 && s.group !== slots[i - 1].group && <span aria-hidden className="mx-1.5 h-8 w-0.5 rounded-full bg-w-line2" />}
          <button
            type="button"
            onClick={s.onClick}
            onPointerDown={
              s.onPress
                ? (e) => {
                    e.currentTarget.setPointerCapture(e.pointerId);
                    s.onPress?.();
                  }
                : undefined
            }
            onPointerUp={s.onRelease}
            onPointerCancel={s.onRelease}
            title={`${s.hint ?? s.label} (${s.key})`}
            aria-label={`${s.label}${s.badge ? `, ${s.badge} new` : ""}`}
            aria-keyshortcuts={s.key}
            className={cn(
              "group relative flex h-[3.25rem] w-[3.75rem] flex-col items-center justify-center gap-1 rounded-xl transition-[transform,background-color,color,filter] duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] active:scale-[0.96] active:duration-100 motion-reduce:transform-none",
              FOCUS,
              "focus-visible:ring-offset-0",
              SLOT_TONE[s.tone ?? "plain"],
            )}
          >
            {s.image ? (
              <img src={s.image} alt="" className="-my-0.5 h-[26px] w-auto transition-transform duration-200 [image-rendering:pixelated] [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transform-none" />
            ) : (
              <s.icon size={22} weight={s.tone === "live" ? "fill" : "duotone"} className="transition-transform duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transform-none" />
            )}
            <span className="max-w-full truncate px-1 text-w-3xs font-semibold leading-none">{s.label}</span>
            {s.badge ? <span className={BADGE}>{s.badge > 99 ? "99+" : s.badge}</span> : null}
          </button>
        </div>
      ))}
    </nav>
  );
}

export function BellButton({ unread, onClick }: { unread: number; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      title="Needs your attention (N)"
      aria-label={`Notifications, ${unread} unread`}
      className={cn(HUD_ICON, "relative")}
    >
      {unread ? <BellRinging key={unread} size={21} weight="duotone" className="origin-top text-w-alert motion-safe:animate-w-ring" /> : <Bell size={21} weight="duotone" className="text-w-text" />}
      {unread > 0 && (
        <span className={BADGE}>
          {unread > 99 ? "99+" : unread}
        </span>
      )}
    </button>
  );
}

export interface Toast {
  id: number;
  text: string;
  kind?: string;
  onClick?: () => void;
}

export function Toasts({ toasts, dismiss }: { toasts: Toast[]; dismiss: (id: number) => void }) {
  useEffect(() => {
    if (!toasts.length) return;
    const id = window.setTimeout(() => dismiss(toasts[0].id), 12000);
    return () => window.clearTimeout(id);
  }, [toasts, dismiss]);
  return (
    <div role="status" aria-live="polite" className="pointer-events-none absolute inset-x-0 top-20 z-40 flex flex-col items-center gap-2 px-3">
      {toasts.map((t) => (
        <button
          key={t.id}
          type="button"
          onClick={() => {
            t.onClick?.();
            dismiss(t.id);
          }}
          className="pointer-events-auto flex max-w-xl animate-w-pop items-center gap-3 rounded-xl border-[3px] border-w-ink bg-w-paper py-2 pl-2 pr-4 text-left text-w-base shadow-[0_4px_0_rgb(var(--w-ink))] transition-[transform,box-shadow,background-color] duration-150 hover:bg-w-field active:translate-y-[3px] active:shadow-none motion-reduce:animate-none motion-reduce:transform-none"
        >
          <GlyphTile glyph={eventGlyph(t.kind ?? "")} size={30} />
          <span className="line-clamp-4 min-w-0 break-words">{t.text}</span>
        </button>
      ))}
    </div>
  );
}

export const HOTBAR_ICONS = { Quick: Lightning, Quests: Scroll, Chat: ChatsTeardrop, History: ClockCounterClockwise, Workshop: Hammer, Main: CrownSimple, Talk: Microphone, Lists: Notepad, Reports: BookOpenText, Terminal: TerminalWindow };
