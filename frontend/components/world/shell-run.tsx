"use client";

import { useEffect, useState } from "react";
import { Check, PaperPlaneTilt, Terminal, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type MessageMeta, type ShellRun } from "@/lib/world";
import { BTN_SECONDARY_SM, CHUNKY } from "./styles";
import { Shimmer } from "./work-ui";
import { CornerCopy } from "./chat-kit";

function fmtDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function StatusBadge({ meta, run }: { meta: MessageMeta; run: ShellRun | null }) {
  const status = run?.status ?? meta.status;
  if (status === "queued") return <Shimmer className="text-w-xs">Waiting for {meta.device ?? "device"}</Shimmer>;
  if (status === "running") return <Shimmer className="text-w-xs">Running on {meta.device ?? "device"}</Shimmer>;
  if (status === "cancelled") return <span className="text-w-xs font-semibold text-w-muted">Cancelled</span>;
  if (status === "failed" && (run?.exit_code ?? meta.exit_code) == null)
    return <span className="text-w-xs font-semibold text-w-danger">Failed</span>;
  const code = run?.exit_code ?? meta.exit_code ?? 0;
  const ms = run?.duration_ms ?? meta.duration_ms;
  return (
    <span className={cn("text-w-xs font-semibold", code === 0 ? "text-w-ok" : "text-w-danger")}>
      exit {code}
      {ms != null ? ` · ${fmtDuration(ms)}` : ""}
    </span>
  );
}

export function ShellRunCard({ meta, body, agentName, canRun }: { meta: MessageMeta; body: string; agentName: string; canRun: boolean }) {
  const runId = meta.shell_run_id;
  const [run, setRun] = useState<ShellRun | null>(null);
  const [busy, setBusy] = useState(false);
  const [sharedHere, setShared] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const active = (run?.status ?? meta.status) === "running" || (run?.status ?? meta.status) === "queued";

  useEffect(() => {
    if (!runId) return;
    let stop = false;
    const pull = () => {
      worldApi
        .shellRun(runId)
        .then((r) => {
          if (!stop) setRun(r);
        })
        .catch(() => {});
    };
    pull();
    if (!active) return;
    const timer = window.setInterval(pull, 1200);
    return () => {
      stop = true;
      window.clearInterval(timer);
    };
  }, [runId, active]);

  const cancel = async () => {
    if (!runId || busy) return;
    setBusy(true);
    setErr(null);
    try {
      const updated = await worldApi.cancelShell(runId);
      setRun(updated);
    } catch {
      setErr("Could not cancel that command.");
    } finally {
      setBusy(false);
    }
  };

  const share = async () => {
    if (!runId || busy || sharedHere || meta.shared || meta.auto_share) return;
    setBusy(true);
    setErr(null);
    try {
      await worldApi.shareShell(runId);
      setShared(true);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Could not share the output.");
    } finally {
      setBusy(false);
    }
  };

  const shared = sharedHere || !!meta.shared || (!active && !!meta.auto_share);
  const output = run && (active || run.output) ? run.output : "";
  const shown = active || !run ? "" : run.truncated || output ? output : body;

  return (
    <div className="max-w-[min(92%,46rem)] rounded-lg border-2 border-w-ink bg-w-card p-3 shadow-[0_3px_0_rgb(var(--w-ink))]">
      <div className="group/code flex items-center gap-2">
        <Terminal size={13} weight="bold" className="shrink-0 text-w-muted" />
        <p className="min-w-0 flex-1 truncate font-mono text-w-sm" title={meta.command ?? ""}>
          $ {meta.command ?? ""}
        </p>
        {meta.command && <CornerCopy text={meta.command} label="Copy command" />}
        <StatusBadge meta={meta} run={run} />
      </div>
      {(active || shown) && (
        <div className="group/code relative mt-2">
          <pre className="max-h-64 overflow-y-auto whitespace-pre-wrap break-words rounded-md border border-w-line2 bg-w-field p-2 pr-10 font-mono text-w-xs leading-relaxed">
            {active ? output || " " : shown}
          </pre>
          {!active && <CornerCopy text={shown} label="Copy output" className="absolute right-1.5 top-1.5" />}
        </div>
      )}
      {!active && run?.error && <p className="mt-1 text-w-sm text-w-danger">{run.error}</p>}
      {err && <p className="mt-1 text-w-sm text-w-danger">{err}</p>}
      <div className="mt-2 flex items-center gap-2">
        {active && meta.auto_share && <span className="text-w-xs text-w-muted">The output goes back to {agentName} when it finishes.</span>}
        {active && canRun && (
          <button type="button" onClick={cancel} disabled={busy} className={cn(CHUNKY, "inline-flex items-center gap-1 bg-w-rose px-2 py-0.5 text-w-xs text-w-danger hover:opacity-90")}>
            <X size={12} weight="bold" />
            Kill it
          </button>
        )}
        {!active && canRun && runId && (
          shared ? (
            <span className="inline-flex items-center gap-1 text-w-xs text-w-ok">
              <Check size={12} weight="bold" />
              Sent to {agentName}
            </span>
          ) : (
            <button
              type="button"
              onClick={share}
              disabled={busy}
              className={cn(BTN_SECONDARY_SM, "inline-flex")}
            >
              <PaperPlaneTilt size={12} weight="bold" />
              Share output with {agentName}
            </button>
          )
        )}
      </div>
    </div>
  );
}
