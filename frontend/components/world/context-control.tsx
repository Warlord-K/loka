"use client";

import { useCallback, useEffect, useId, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowCounterClockwise, ArrowsInSimple } from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cn } from "@/lib/utils";
import { compactNumber } from "@/lib/world";
import {
  AUTO_CONTEXT_MAX_TOKENS,
  AUTO_CONTEXT_MIN_TOKENS,
  CONTEXT_EVENT,
  CONTEXT_MISSING,
  CONTEXT_WARN_RATIO,
  compactAgentContext,
  contextRatio,
  defaultAutoTokens,
  fetchAgentContext,
  patchAgentContext,
  readContext,
  resetAgentContext,
  type AgentContext,
  type AutoContextAction,
  type AutoContextPatch,
  type ContextRequest,
} from "@/lib/agent-context";
import type { WorldEvent } from "@/lib/world";
import { Shimmer } from "./shimmer";
import { qk } from "@/lib/query-keys";
import { BTN, BTN_SM, BTN_SECONDARY_SM, SEGMENT, SEGMENT_ITEM, SEGMENT_OFF, SEGMENT_ON } from "./styles";
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
    window.addEventListener("world-reconnected", refresh);
    return () => {
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener("world-reconnected", refresh);
    };
  }, [agentId, enabled, client, refresh]);
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

const AUTO_ACTIONS: { value: AutoContextAction; label: string }[] = [
  { value: "compact", label: "Compact" },
  { value: "reset", label: "Reset" },
];

function tokenText(n: number | null): string {
  return n == null ? "" : n.toLocaleString();
}

function AutoContextRow({ agentId, agentName, context }: { agentId: string; agentName: string; context: AgentContext }) {
  const client = useQueryClient();
  const inputId = useId();
  const saved = context.auto_context_tokens;
  const action = context.auto_context_action;
  const max = context.max_tokens;
  const [draft, setDraft] = useState(tokenText(saved));
  const [error, setError] = useState<string | null>(null);
  const [savingTokens, setSavingTokens] = useState(0);
  const queue = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    setDraft(tokenText(saved));
  }, [saved]);

  const send = async (patch: AutoContextPatch) => {
    const tokens = "auto_context_tokens" in patch;
    if (tokens) setSavingTokens((n) => n + 1);
    setError(null);
    const result = await patchAgentContext(agentId, patch);
    if (tokens) setSavingTokens((n) => n - 1);
    if (result.ok) {
      client.setQueryData(qk.agent.context(agentId), result.context);
      setDraft(tokenText(result.context.auto_context_tokens));
    } else {
      setError(result.error);
    }
  };

  const save = (patch: AutoContextPatch) => {
    queue.current = queue.current.then(() => send(patch));
    return queue.current;
  };

  const commit = () => {
    const digits = draft.replace(/\D/g, "");
    const tokens = digits ? Number(digits) : null;
    if (tokens === saved) {
      setError(null);
      setDraft(tokenText(saved));
      return;
    }
    if (tokens !== null && tokens < AUTO_CONTEXT_MIN_TOKENS) {
      setError(`Use at least ${AUTO_CONTEXT_MIN_TOKENS.toLocaleString()} tokens, or use the default.`);
      return;
    }
    if (tokens !== null && max && tokens >= max) {
      setError(`Keep it below ${max.toLocaleString()}, the model's context window.`);
      return;
    }
    if (tokens !== null && !max && tokens > AUTO_CONTEXT_MAX_TOKENS) {
      setError(`Keep it at or below ${AUTO_CONTEXT_MAX_TOKENS.toLocaleString()} tokens.`);
      return;
    }
    void save({ auto_context_tokens: tokens });
  };

  const fallback = defaultAutoTokens(context);
  const verb = action === "compact" ? "compacts" : "resets";
  const summary =
    saved === null
      ? fallback
        ? `Default, about ${fallback.toLocaleString()} tokens. After a turn past it, ${agentName} ${verb} on their own.`
        : "Default. Enter a token count to set your own limit."
      : `After a turn past ${saved.toLocaleString()} tokens, ${agentName} ${verb} on their own.`;

  return (
    <div className="space-y-2 border-t-2 border-w-line pt-2.5">
      <div className="flex items-center justify-between gap-2">
        <label htmlFor={inputId} className="text-w-xs font-semibold text-w-strong">
          Auto limit
        </label>
        <div role="radiogroup" aria-label="When the limit is passed" className={cn(SEGMENT, "shrink-0")}>
          {AUTO_ACTIONS.map((a) => (
            <button
              key={a.value}
              type="button"
              role="radio"
              aria-checked={action === a.value}
              onClick={() => {
                if (action !== a.value) void save({ auto_context_action: a.value });
              }}
              className={cn(SEGMENT_ITEM, "px-2 py-0.5 motion-reduce:transition-none", action === a.value ? SEGMENT_ON : SEGMENT_OFF)}
            >
              {a.label}
            </button>
          ))}
        </div>
      </div>
      <div className="flex items-center gap-2">
        <input
          id={inputId}
          type="text"
          inputMode="numeric"
          autoComplete="off"
          placeholder={fallback ? `Default, about ${fallback.toLocaleString()}` : "Default"}
          value={draft}
          disabled={savingTokens > 0}
          aria-invalid={!!error}
          aria-describedby={`${inputId}-note`}
          onChange={(e) => {
            setDraft(e.target.value);
            setError(null);
          }}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commit();
            }
          }}
          className="h-8 w-24 min-w-0 rounded-lg border-2 border-w-ink bg-w-field px-2 text-w-sm tabular-nums outline-none transition-shadow duration-150 placeholder:text-w-faint focus:ring-2 focus:ring-w-teal disabled:opacity-60 motion-reduce:transition-none"
        />
        <span className="min-w-0 flex-1 truncate text-w-xs tabular-nums text-w-muted">{max ? `of ${max.toLocaleString()}` : "window unknown"}</span>
        {saved !== null && (
          <button type="button" onClick={() => void save({ auto_context_tokens: null })} className={cn(BTN_SECONDARY_SM, "shrink-0")}>
            Use default
          </button>
        )}
      </div>
      <p id={`${inputId}-note`} className={cn("text-w-xs leading-relaxed", error ? "text-w-alert" : "text-w-muted")} role={error ? "alert" : undefined}>
        {error ?? summary}
      </p>
      {!error && action === "compact" && !context.can_compact && (
        <p className="text-w-xs leading-relaxed text-w-muted">This CLI cannot compact, so it resets instead.</p>
      )}
    </div>
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
            {context && stage === "menu" && <AutoContextRow agentId={agentId} agentName={agentName} context={context} />}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
