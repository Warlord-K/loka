"use client";

import { Hammer, HandWaving, WarningCircle } from "@phosphor-icons/react";
import { Shimmer } from "./shimmer";
import { useNow } from "@/lib/clock";
import { cn } from "@/lib/utils";
import { TRADES, WAITING_FOR_DISK, type AgentJob, type WorldAgent } from "@/lib/world";
import type { Trade } from "./types";
import { OpenWorkshop } from "./workbench/open-workshop";

const KIND_LABEL: Record<AgentJob["kind"], string> = {
  task: "Building",
  review: "Under review",
  followup: "Follow-up",
  publish: "Shipping",
};

export { Shimmer };

export function elapsed(since: string, now: number): string {
  const s = Math.max(0, Math.floor((now - new Date(since).getTime()) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function tradeVerb(trade: Trade): string {
  return TRADES.find((t) => t.id === trade)?.verb ?? "Working";
}

export function JobList({
  agent,
  trade,
  className,
  onOpen,
}: {
  agent: WorldAgent;
  trade: Trade;
  className?: string;
  onOpen?: (job: AgentJob) => void;
}) {
  const jobs = agent.jobs ?? [];
  const now = useNow(1000, jobs.length > 0);
  if (!jobs.length) return null;
  return (
    <div className={cn("rounded-lg border-2 border-w-ink/15 bg-w-card/80 p-2", className)}>
      <p className="mb-1.5 flex items-center gap-1.5 text-w-xs font-semibold text-w-muted">
        <span className="animate-pop-in">{tradeVerb(trade)}</span>
        <span key={jobs.length} className="inline-flex h-4 min-w-4 animate-pop-in items-center justify-center rounded-[0.25rem] bg-w-invert px-1 text-w-3xs text-w-oninvert">
          {jobs.length}
        </span>
      </p>
      <ul className="space-y-1.5">
        {jobs.map((job) => {
          const queued = job.status === "queued";
          return (
            <li key={job.run_id} className="flex animate-job-in items-start gap-1">
              <button
                type="button"
                disabled={!onOpen}
                onClick={() => onOpen?.(job)}
                title={onOpen ? "Open this session" : undefined}
                className="-ml-1 flex min-w-0 flex-1 items-start gap-2 rounded-md px-1 py-0.5 text-left text-w-sm transition-colors enabled:hover:bg-w-sand disabled:cursor-default"
              >
              <span className="min-w-0 flex-1">
                <span className="flex items-baseline gap-1.5">
                  <span className="truncate font-semibold text-w-strong">{job.title ?? "Untitled quest"}</span>
                  <span className="shrink-0 text-w-xs font-medium text-w-muted">{KIND_LABEL[job.kind] ?? job.kind}</span>
                  <span className="ml-auto shrink-0 font-mono text-w-2xs tabular-nums text-w-muted">{elapsed(job.started_at, now)}</span>
                </span>
                <span key={job.activity ?? (queued ? "queued" : "starting")} className="block animate-fade-in truncate text-w-xs">
                  {queued ? (
                    <span className="text-w-muted">{job.waiting === "disk" ? WAITING_FOR_DISK : "Waiting for a free workbench"}</span>
                  ) : (
                    <Shimmer>{job.activity ?? "Getting started"}</Shimmer>
                  )}
                </span>
              </span>
              </button>
              {(job.session_id || job.task_id) && (
                <OpenWorkshop
                  className="mt-0.5 shrink-0"
                  detail={{ sessionId: job.kind === "review" ? null : job.session_id ?? null, taskId: job.task_id, agentId: agent.id }}
                />
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export function LiveJobLine({ job }: { job: AgentJob }) {
  const now = useNow(1000);
  const queued = job.status === "queued";
  return (
    <p className="mt-1.5 flex items-center gap-2 text-w-sm">
      <span className="min-w-0 flex-1 truncate">
        {queued ? <span className="text-w-muted">{job.waiting === "disk" ? WAITING_FOR_DISK : "Waiting for a free workbench"}</span> : <Shimmer>{job.activity ?? "Working on it"}</Shimmer>}
      </span>
      <span className="shrink-0 font-mono text-w-2xs tabular-nums text-w-muted">{elapsed(job.started_at, now)}</span>
    </p>
  );
}

export const STATUS_LABEL: Record<WorldAgent["status"], string> = {
  idle: "Hanging around",
  thinking: "Thinking",
  away: "Out on a quest",
  waiting: "Waiting on you",
  error: "Something went wrong",
};

export function AgentStatus({ agent, size = 12, detail = true }: { agent: WorldAgent; size?: number; detail?: boolean }) {
  return (
    <>
      {agent.status === "thinking" ? (
        <Shimmer className="shrink-0">{STATUS_LABEL.thinking}</Shimmer>
      ) : (
        <span className="flex shrink-0 items-center gap-1.5">
          <StatusMark status={agent.status} working={!!agent.jobs?.length} size={size} />
          {agent.status === "idle" && agent.jobs?.length ? "Free to chat" : STATUS_LABEL[agent.status]}
        </span>
      )}
      {detail && agent.status_detail && agent.status !== "error" && <span className="min-w-0 truncate">· {agent.status_detail}</span>}
    </>
  );
}

export function StatusMark({ status, working = false, size = 13, className }: { status: string; working?: boolean; size?: number; className?: string }) {
  if (status === "thinking") return <Shimmer className={cn("shrink-0 text-w-3xs", className)}>thinking</Shimmer>;
  if (status === "error") return <WarningCircle size={size} weight="bold" aria-label="Something went wrong" className={cn("shrink-0 text-w-danger", className)} />;
  if (status === "waiting") return <HandWaving size={size} weight="bold" aria-label="Waiting on you" className={cn("shrink-0 text-w-alert", className)} />;
  if (working) return <Hammer size={size} weight="bold" aria-label="On a quest" className={cn("shrink-0 text-w-link", className)} />;
  return null;
}
