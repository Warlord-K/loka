"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowCounterClockwise, ArrowsInSimple } from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cn } from "@/lib/utils";
import { compactNumber } from "@/lib/world";
import {
  CONTEXT_EVENT,
  CONTEXT_MISSING,
  CONTEXT_WARN_RATIO,
  compactAgentContext,
  contextRatio,
  fetchAgentContext,
  readContext,
  resetAgentContext,
  type AgentContext,
  type ContextRequest,
} from "@/lib/agent-context";
import type { WorldEvent } from "@/lib/world";
import { Shimmer } from "./shimmer";
import { qk } from "@/lib/query-keys";
import { BTN, BTN_SM, BTN_SECONDARY_SM } from "./styles";
import { GHOST_ICON_BTN } from "./settings-shell";
import { MENU } from "./controls";
import { POPOVER_MOTION } from "./motion";

function short(n: number): string {
  return compactNumber(n).toLowerCase();
}

export function useAgentContext(agentId: string, enabled: boolean, refreshKey: string) {
  const client = useQueryClient();
  const missing = client.getQueryData(qk.agent.context(agentId)) === CONTEXT_MISSING;
  const live = enabled && !missing;
  const query = useQuery({
    queryKey: qk.agent.context(agentId),
    queryFn: () => fetchAgentContext(agentId),
    enabled: live,
    retry: false,
    staleTime: 15_000,
    refetchOnWindowFocus: false,
  });
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    if (live) void client.invalidateQueries({ queryKey: qk.agent.context(agentId) });
  }, [agentId, live, refreshKey, client]);
  const refresh = useCallback(() => {
    if (live) void client.invalidateQueries({ queryKey: qk.agent.context(agentId) });
  }, [agentId, live, client]);
  useEffect(() => {
    if (!enabled) return;
    const onEvent = (e: Event) => {
      const ev = (e as CustomEvent<WorldEvent>).detail;
      if (ev.type !== CONTEXT_EVENT || ev.agent_id !== agentId) return;
      const next = readContext(ev.payload);
      if (next) client.setQueryData(qk.agent.context(agentId), next);
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [agentId, enabled, client]);
  const data = query.data;
  const context = enabled && data && data !== CONTEXT_MISSING ? data : null;
  return { context, refresh };
}

function pendingText(context: AgentContext): string | null {
  if (context.pending === "compact") return "Compacting";
  if (context.pending === "reset") return "Resetting";
  return null;
}

export function ContextMeter({ context, className }: { context: AgentContext | null; className?: string }) {
  if (!context) return null;
  const busy = pendingText(context);
  if (busy) {
    return (
      <span className={cn("inline-flex shrink-0 items-center", className)} title={`Context: ${busy.toLowerCase()}`}>
        <Shimmer>{busy}</Shimmer>
      </span>
    );
  }
  if (context.used_tokens === 0) {
    return (
      <span className={cn("inline-flex shrink-0 items-center text-w-muted", className)} title="Context: fresh, nothing used yet">
        fresh
      </span>
    );
  }
  const ratio = contextRatio(context);
  if (ratio === null || !context.max_tokens) {
    return (
      <span className={cn("inline-flex shrink-0 items-center tabular-nums", className)} title={`Context: ${context.used_tokens.toLocaleString()} tokens used`}>
        {short(context.used_tokens)} used
      </span>
    );
  }
  const warn = ratio >= CONTEXT_WARN_RATIO;
  const percent = Math.round(ratio * 100);
  return (
    <span
      className={cn("inline-flex shrink-0 items-center gap-1.5 tabular-nums", warn && "text-w-warn", className)}
      title={`Context: ${context.used_tokens.toLocaleString()} of ${context.max_tokens.toLocaleString()} tokens used (${percent}%)`}
    >
      <span>
        {short(context.used_tokens)} / {short(context.max_tokens)}
      </span>
      <span
        role="meter"
        aria-label="Context used"
        aria-valuemin={0}
        aria-valuemax={context.max_tokens}
        aria-valuenow={context.used_tokens}
        className="relative h-1.5 w-10 overflow-hidden rounded-[0.15rem] bg-w-line2"
      >
        <span
          className={cn(
            "absolute inset-y-0 left-0 rounded-[0.15rem] transition-[width,background-color] duration-500 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none",
            warn ? "bg-w-warn" : "bg-w-teal",
          )}
          style={{ width: `${Math.max(percent, ratio > 0 ? 4 : 0)}%` }}
        />
      </span>
    </span>
  );
}

type Stage = "menu" | "confirm";

export function ContextButton({
  agentId,
  agentName,
  context,
  onChanged,
  className,
}: {
  agentId: string;
  agentName: string;
  context: AgentContext | null;
  onChanged: (kind: "compact" | "reset", result: ContextRequest) => void;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [stage, setStage] = useState<Stage>("menu");
  const [busy, setBusy] = useState<"compact" | "reset" | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const canCompact = !!context?.compact_supported;
  const pending = context?.pending ?? null;
  const label = canCompact ? "Compact or reset context" : "Reset context";

  const close = useCallback(() => {
    setOpen(false);
    setStage("menu");
  }, []);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (root.current && !root.current.contains(e.target as Node)) close();
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  }, [open, close]);

  const compact = async () => {
    setBusy("compact");
    setFailed(false);
    try {
      const result = await compactAgentContext(agentId);
      if ("refused" in result) {
        setNote(result.refused);
        setFailed(true);
      } else {
        setNote(result.queued ? `Compacting. ${agentName} keeps a summary of the conversation and frees up room.` : "A compaction is already queued.");
      }
      onChanged("compact", result);
    } catch {
      setNote("Could not compact right now.");
      setFailed(true);
    } finally {
      setBusy(null);
    }
  };

  const reset = async () => {
    setBusy("reset");
    setFailed(false);
    try {
      const result = await resetAgentContext(agentId);
      if ("refused" in result) {
        setNote(result.refused);
        setFailed(true);
      } else {
        setNote(result.queued ? `Reset queued. ${agentName} first rewrites their notes, then starts fresh. Old messages stay visible.` : "A reset is already queued.");
      }
      setStage("menu");
      onChanged("reset", result);
    } catch {
      setNote("Could not queue the reset.");
      setFailed(true);
      setStage("menu");
    } finally {
      setBusy(null);
    }
  };

  return (
    <div
      ref={root}
      className={cn("relative", className)}
      onKeyDown={(e) => {
        if (e.key === "Escape" && open) {
          e.preventDefault();
          e.stopPropagation();
          close();
        }
      }}
    >
      <button
        type="button"
        onClick={() => {
          setNote(null);
          setFailed(false);
          setStage("menu");
          setOpen((v) => !v);
        }}
        aria-label={label}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={label}
        className={cn(GHOST_ICON_BTN, open && "bg-w-sand text-w-strong")}
      >
        {canCompact ? <ArrowsInSimple size={18} /> : <ArrowCounterClockwise size={18} />}
      </button>
      <AnimatePresence>
        {open && (
          <motion.div
            key="context-menu"
            role="dialog"
            aria-label={label}
            {...POPOVER_MOTION}
            className={cn(MENU, "absolute right-0 top-full z-30 mt-2 w-[19rem] origin-top-right space-y-2 p-3")}
          >
            <p className="flex items-center justify-between gap-2 text-w-xs text-w-muted">
              <span className="font-semibold text-w-strong">Context</span>
              {context && <ContextMeter context={context} />}
            </p>
            {!note && pending && (
              <p className="text-w-sm leading-relaxed text-w-text2">
                {pending === "compact" ? `A compaction is queued. ${agentName} runs it after the current turn.` : `A reset is queued. ${agentName} runs it after the current turn.`}
              </p>
            )}
            {note && <p className={cn("text-w-sm leading-relaxed", failed ? "text-w-alert" : "text-w-text2")}>{note}</p>}
            {(note && !failed) || pending ? null : stage === "confirm" ? (
              <>
                <p className="text-w-sm leading-relaxed text-w-text2">
                  Really reset {agentName}&apos;s mind? Their notes get rewritten first, then the conversation memory starts over.
                </p>
                <div className="flex justify-end gap-2">
                  <button type="button" disabled={!!busy} onClick={() => setStage("menu")} className={BTN_SECONDARY_SM}>
                    Cancel
                  </button>
                  <button type="button" disabled={!!busy} onClick={() => void reset()} className={cn(BTN, BTN_SM, "bg-w-alert text-w-onalert hover:bg-w-coral")}>
                    {busy === "reset" ? "Resetting" : "Yes, reset"}
                  </button>
                </div>
              </>
            ) : (
              <div className="space-y-2">
                {!canCompact && context?.compact_unsupported_reason && (
                  <p className="text-w-xs leading-relaxed text-w-muted">{context.compact_unsupported_reason}</p>
                )}
                {canCompact && (
                  <div className="flex items-center gap-2.5">
                    <p className="min-w-0 flex-1 text-w-xs leading-relaxed text-w-text2">Summarize the conversation so far and keep going with more room.</p>
                    <button type="button" disabled={!!busy} onClick={() => void compact()} className={cn(BTN_SECONDARY_SM, "shrink-0")}>
                      <ArrowsInSimple size={13} weight="bold" />
                      {busy === "compact" ? "Compacting" : "Compact"}
                    </button>
                  </div>
                )}
                <div className="flex items-center gap-2.5">
                  <p className="min-w-0 flex-1 text-w-xs leading-relaxed text-w-text2">Notes are rewritten first, then the chat memory starts over.</p>
                  <button type="button" disabled={!!busy} onClick={() => setStage("confirm")} className={cn(BTN_SECONDARY_SM, "shrink-0")}>
                    <ArrowCounterClockwise size={13} weight="bold" />
                    Reset
                  </button>
                </div>
              </div>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
