"use client";

import { useState } from "react";
import { ArrowsClockwise, Check, GitMerge, Plus, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { messageTime, worldApi, type Milestone, type MilestoneDraft } from "@/lib/world";
import { INPUT } from "./styles";

const MERGE_TITLE = "Get the PR reviewed and merged";

export function milestoneProgress(milestones: Milestone[] | undefined): string | null {
  const counted = (milestones ?? []).filter((m) => m.status !== "skipped");
  if (!counted.length) return null;
  return `${counted.filter((m) => m.status === "done").length}/${counted.length}`;
}

function mergeLine(m: Milestone): string {
  if (m.status === "done") return "Merged";
  if (m.status === "failed") return m.detail ?? "Closed without merging";
  if (m.status === "skipped") return m.detail ?? "Skipped";
  if (m.status === "pending") return "Starts when the PR opens";
  const next = m.next_check_at ? `, next check ${messageTime(m.next_check_at)}` : "";
  return `${m.detail ?? "Waiting for review"}${next}`;
}

export function MilestoneList({
  taskId,
  milestones,
  editable,
  onChanged,
  className,
}: {
  taskId: string;
  milestones: Milestone[];
  editable: boolean;
  onChanged: () => void;
  className?: string;
}) {
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const act = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
      onChanged();
    } catch {
      return;
    } finally {
      setBusy(false);
    }
  };
  if (!milestones.length && !editable) return null;
  const hasMerge = milestones.some((m) => m.kind === "merge");
  return (
    <div className={cn("space-y-1", className)} onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
      {milestones.length > 0 && (
        <ul className="space-y-1">
          {milestones.map((m) => {
            const done = m.status === "done";
            const merge = m.kind === "merge";
            return (
              <li key={m.id} className="group/ms flex items-start gap-2 text-w-xs">
                {merge ? (
                  <span
                    className={cn(
                      "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-md border-2 border-w-ink",
                      done ? "bg-w-ok text-w-paper" : m.status === "failed" ? "bg-w-alert text-w-onalert" : "bg-w-field",
                    )}
                    aria-hidden
                  >
                    <GitMerge size={10} weight="bold" />
                  </span>
                ) : (
                  <button
                    type="button"
                    disabled={!editable || busy}
                    onClick={() => void act(() => worldApi.updateMilestone(m.id, { status: done ? "pending" : "done" }))}
                    aria-label={done ? `Mark "${m.title}" not done` : `Mark "${m.title}" done`}
                    className={cn(
                      "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-md border-2 border-w-ink transition-colors",
                      done ? "bg-w-ok text-w-paper" : "bg-w-field hover:bg-w-sand",
                    )}
                  >
                    {done && <Check size={10} weight="bold" />}
                  </button>
                )}
                <span className="min-w-0 flex-1">
                  <span className={cn("block leading-snug", done && "text-w-muted line-through")}>{m.title}</span>
                  {merge && <span className="block text-w-2xs text-w-muted">{mergeLine(m)}</span>}
                </span>
                {merge && m.status === "active" && editable && (
                  <button
                    type="button"
                    disabled={checking}
                    onClick={() => {
                      setChecking(true);
                      void worldApi.checkMilestones(taskId).finally(() => window.setTimeout(() => setChecking(false), 60000));
                    }}
                    title="Check the PR on the next sweep, within a minute"
                    className="inline-flex shrink-0 items-center gap-1 rounded-md px-1 text-w-2xs text-w-link hover:bg-w-sand disabled:text-w-muted"
                  >
                    <ArrowsClockwise size={11} weight="bold" />
                    {checking ? "Checking soon" : "Check now"}
                  </button>
                )}
                {editable && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void act(() => worldApi.deleteMilestone(m.id))}
                    aria-label={`Remove "${m.title}"`}
                    className="shrink-0 rounded-md p-0.5 text-w-muted opacity-0 hover:bg-w-sand hover:text-w-strong focus-visible:opacity-100 group-hover/ms:opacity-100 [@media(hover:none)]:opacity-100"
                  >
                    <X size={11} weight="bold" />
                  </button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {editable && (
        <form
          className="flex flex-wrap items-center gap-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            const title = draft.trim();
            if (!title) return;
            setDraft("");
            void act(() => worldApi.addMilestones(taskId, [{ title, kind: "work" }]));
          }}
        >
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="Add a milestone"
            aria-label="New milestone"
            className={cn(INPUT, "h-7 min-w-0 flex-1 py-0 text-w-xs")}
          />
          {!hasMerge && (
            <button
              type="button"
              disabled={busy}
              onClick={() => void act(() => worldApi.addMilestones(taskId, [{ title: MERGE_TITLE, kind: "merge" }]))}
              className="inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-w-xs text-w-link hover:bg-w-sand"
            >
              <GitMerge size={11} weight="bold" />
              Wait for merge
            </button>
          )}
        </form>
      )}
    </div>
  );
}

export function MilestoneDraftEditor({
  steps,
  onSteps,
  waitForMerge,
  onWaitForMerge,
  hasRepo,
}: {
  steps: string[];
  onSteps: (steps: string[]) => void;
  waitForMerge: boolean;
  onWaitForMerge: (value: boolean) => void;
  hasRepo: boolean;
}) {
  const [draft, setDraft] = useState("");
  const add = () => {
    const title = draft.trim();
    if (!title) return;
    onSteps([...steps, title]);
    setDraft("");
  };
  return (
    <div className="space-y-1.5 rounded-md border-2 border-dashed border-w-line2 p-2">
      <p className="text-w-xs font-semibold text-w-muted">Milestones</p>
      {steps.length > 0 && (
        <ol className="space-y-1">
          {steps.map((step, i) => (
            <li key={`${i}-${step}`} className="flex items-center gap-2 text-w-sm">
              <span className="w-4 shrink-0 text-right font-mono text-w-2xs text-w-muted">{i + 1}.</span>
              <span className="min-w-0 flex-1 truncate">{step}</span>
              <button
                type="button"
                onClick={() => onSteps(steps.filter((_, j) => j !== i))}
                aria-label={`Remove "${step}"`}
                className="rounded-md p-0.5 text-w-muted hover:bg-w-sand hover:text-w-strong"
              >
                <X size={11} weight="bold" />
              </button>
            </li>
          ))}
        </ol>
      )}
      <div className="flex gap-1.5">
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          placeholder="Add a step (optional)"
          aria-label="New milestone step"
          className={cn(INPUT, "h-8 min-w-0 flex-1 py-0 text-w-sm")}
        />
        <button type="button" onClick={add} disabled={!draft.trim()} aria-label="Add step" className="rounded-md border-2 border-w-ink bg-w-field px-2 hover:bg-w-sand disabled:opacity-50">
          <Plus size={12} weight="bold" />
        </button>
      </div>
      <label className={cn("flex items-center gap-2 text-w-sm", !hasRepo && "text-w-muted")}>
        <input type="checkbox" checked={hasRepo && waitForMerge} disabled={!hasRepo} onChange={(e) => onWaitForMerge(e.target.checked)} className="accent-w-teal" />
        Wait for review and merge {hasRepo ? "(checks the PR every 30 minutes)" : "(needs a repo)"}
      </label>
    </div>
  );
}

export function draftsFrom(steps: string[]): MilestoneDraft[] {
  return steps.map((title) => ({ title, kind: "work" }));
}
