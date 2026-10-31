"use client";

import {
  BellSimple,
  DownloadSimple,
  BookOpenText,
  ChatTeardropDots,
  CloudLightning,
  Coins,
  Confetti,
  GitMerge,
  GitPullRequest,
  HandWaving,
  Handshake,
  Hourglass,
  Lighthouse,
  MapTrifold,
  Medal,
  PushPinSimple,
  Signpost,
  Sparkle,
  Student,
  Timer,
  TreasureChest,
  type Icon,
} from "@phosphor-icons/react";
import type { WorldAgent } from "@/lib/world";
import { cn } from "@/lib/utils";
import { AgentFace } from "./channel-ui";

export type GlyphTone = "ok" | "warn" | "danger" | "info" | "quiet" | "gold";

export interface EventGlyph {
  icon: Icon;
  tone: GlyphTone;
  label: string;
}

const TONE: Record<GlyphTone, string> = {
  ok: "bg-w-mint text-w-ok",
  warn: "bg-w-lemon text-w-warn",
  danger: "bg-w-rose text-w-danger",
  info: "bg-w-aqua text-w-link",
  quiet: "bg-w-sand text-w-text2",
  gold: "bg-w-crown text-w-ink",
};

export function eventGlyph(type: string, error = false): EventGlyph {
  switch (type) {
    case "task.finished":
      return error ? { icon: CloudLightning, tone: "danger", label: "Quest ran into trouble" } : { icon: Confetti, tone: "ok", label: "Quest finished" };
    case "task.merged":
      return { icon: GitMerge, tone: "ok", label: "Merged" };
    case "task.pr_closed":
      return { icon: GitPullRequest, tone: "danger", label: "Pull request closed" };
    case "task.plan_ready":
      return { icon: MapTrifold, tone: "warn", label: "Plan ready" };
    case "run.failed":
      return { icon: CloudLightning, tone: "danger", label: "Run failed" };
    case "run.timed_out":
      return { icon: Timer, tone: "danger", label: "Ran out of time" };
    case "agent.status":
      return { icon: Lighthouse, tone: "danger", label: "Needs a fix" };
    case "attention.requested":
      return { icon: HandWaving, tone: "warn", label: "Asking you" };
    case "timer.done":
      return { icon: Hourglass, tone: "warn", label: "Timer" };
    case "todo.nudge":
      return { icon: PushPinSimple, tone: "info", label: "To do" };
    case "report.created":
      return { icon: BookOpenText, tone: "info", label: "Report" };
    case "agent.budget":
    case "org.budget":
      return { icon: Coins, tone: "gold", label: "Budget" };
    case "agent.transferred":
      return { icon: Handshake, tone: "quiet", label: "Handed over" };
    case "message.created":
    case "channel.message":
    case "aside.answered":
      return { icon: ChatTeardropDots, tone: "info", label: "Message" };
    case "changelog":
      return { icon: Sparkle, tone: "gold", label: "What's new" };
    case "download":
      return { icon: DownloadSimple, tone: "ok", label: "Downloaded" };
    case "travel":
      return { icon: Signpost, tone: "warn", label: "Travel" };
    case "challenge.passed":
      return { icon: Medal, tone: "gold", label: "Passed" };
    case "challenge.graded":
      return { icon: Student, tone: "info", label: "Graded" };
    case "academy.unlocked":
      return { icon: TreasureChest, tone: "gold", label: "Unlocked" };
    default:
      return { icon: BellSimple, tone: "quiet", label: "Notice" };
  }
}

export function GlyphTile({ glyph, size = 28, className }: { glyph: EventGlyph; size?: number; className?: string }) {
  const G = glyph.icon;
  return (
    <span
      role="img"
      aria-label={glyph.label}
      style={{ width: size, height: size }}
      className={cn("grid shrink-0 place-items-center rounded-lg border-2 border-w-ink shadow-[0_2px_0_rgb(var(--w-ink))]", TONE[glyph.tone], className)}
    >
      <G size={Math.round(size * 0.55)} weight="duotone" />
    </span>
  );
}

export function EventMark({ glyph, agent, size = 32, className }: { glyph: EventGlyph; agent?: WorldAgent | null; size?: number; className?: string }) {
  if (!agent) return <GlyphTile glyph={glyph} size={size} className={className} />;
  const G = glyph.icon;
  const badge = Math.round(size * 0.6);
  return (
    <span role="img" aria-label={`${agent.name}: ${glyph.label}`} className={cn("relative mt-px inline-block shrink-0", className)} style={{ width: size, height: size }}>
      <AgentFace agent={agent} size={size} soft={false} />
      <span
        style={{ width: badge, height: badge }}
        className={cn("absolute -bottom-1.5 -right-1.5 grid place-items-center rounded-md border-2 border-w-ink", TONE[glyph.tone])}
      >
        <G size={Math.round(badge * 0.66)} weight="fill" />
      </span>
    </span>
  );
}
