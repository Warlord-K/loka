"use client";

import { useCallback, useEffect, useState } from "react";
import { Broom, Database } from "@phosphor-icons/react";
import { WSelect } from "./controls";
import { EmptyState, InfoNote, SettingsCard, SettingsGroup, SettingsRow } from "./settings-shell";
import { BTN_DANGER, BTN_SECONDARY } from "./styles";
import { RelativeTime } from "./relative-time";
import { Shimmer } from "./work-ui";

type CleanupKind = "run_logs" | "quests" | "chats" | "activity";

type AgentStorage = {
  agent_id: string;
  name: string;
  slug: string;
  messages: number;
  message_bytes: number;
  runs: number;
  run_bytes: number;
  steps: number;
  step_bytes: number;
  events: number;
  event_bytes: number;
  total_bytes: number;
  last_active: string | null;
};

type PastCleanup = {
  kind: CleanupKind;
  agent_id: string | null;
  by_agent_id: string | null;
  older_than: number;
  rows_removed: number;
  bytes_freed: number;
  created_at: string;
};

type Report = { agents: AgentStorage[]; total_bytes: number; cleanups: PastCleanup[] };

type Result = { kind: CleanupKind; older_than_days: number; agent_id: string | null; rows: number; bytes: number; applied: boolean };

const KINDS: { kind: CleanupKind; title: string; description: string }[] = [
  { kind: "run_logs", title: "Run step logs", description: "Every step, prompt and raw output of finished runs. Usually the biggest part. Runs, costs and chats stay." },
  { kind: "quests", title: "Closed quests", description: "Done, failed and cancelled quests with their sessions, runs and logs. Lifetime cost totals stay." },
  { kind: "chats", title: "Old chat messages", description: "Messages between you, other people and your agents. Agents keep what they remember in their own sessions and notes." },
  { kind: "activity", title: "Old activity", description: "The activity feed behind quest rooms and the bell, including cleared bell items." },
];

const AGES = [
  { value: "30", label: "Older than 30 days" },
  { value: "90", label: "Older than 90 days" },
  { value: "180", label: "Older than 6 months" },
  { value: "365", label: "Older than a year" },
];

const LABEL: Record<CleanupKind, string> = { run_logs: "Run step logs", quests: "Closed quests", chats: "Chat messages", activity: "Activity" };

function size(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`;
  if (bytes >= 1e3) return `${Math.round(bytes / 1e3)} KB`;
  return `${Math.max(0, bytes)} B`;
}

async function readError(r: Response): Promise<string> {
  const body = (await r.json().catch(() => null)) as { error?: string } | null;
  return body?.error ?? "Something went wrong. Try again in a minute.";
}

function Cleanup({ kind, title, description, agents, onDone }: { kind: CleanupKind; title: string; description: string; agents: AgentStorage[]; onDone: () => void }) {
  const [days, setDays] = useState("90");
  const [agent, setAgent] = useState("");
  const [preview, setPreview] = useState<Result | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  useEffect(() => setPreview(null), [days, agent]);
  const run = async (apply: boolean) => {
    setBusy(true);
    setNote(null);
    try {
      const r = await fetch("/api/world/storage/cleanup", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ kind, older_than_days: Number(days), agent_id: agent || null, apply }),
      });
      if (!r.ok) {
        setNote(await readError(r));
        return;
      }
      const result = (await r.json()) as Result;
      if (apply) {
        setPreview(null);
        setNote(result.applied ? `Deleted ${result.rows.toLocaleString()} items, about ${size(result.bytes)} freed.` : "Nothing was left to delete.");
        onDone();
      } else {
        setPreview(result);
      }
    } catch {
      setNote("Could not reach Loka. Nothing was removed.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <SettingsRow title={title} description={description}>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <WSelect value={days} onChange={setDays} options={AGES} ariaLabel={`${title}: how old`} size="sm" disabled={busy} />
        <WSelect
          value={agent || "all"}
          onChange={(v) => setAgent(v === "all" ? "" : v)}
          options={[{ value: "all", label: "All my agents" }, ...agents.map((a) => ({ value: a.agent_id, label: a.name }))]}
          ariaLabel={`${title}: which agent`}
          size="sm"
          disabled={busy}
        />
        {!preview && (
          <button type="button" className={BTN_SECONDARY} onClick={() => void run(false)} disabled={busy}>
            {busy ? <Shimmer>Checking</Shimmer> : "Check what would go"}
          </button>
        )}
      </div>
      {preview && (
        <div className="mt-3 rounded-lg border-2 border-dashed border-w-line2 px-3 py-2.5">
          {preview.rows === 0 ? (
            <p className="text-w-sm text-w-muted">Nothing that old. There is nothing to delete here.</p>
          ) : (
            <>
              <p className="text-w-sm text-w-text">
                {preview.rows.toLocaleString()} items, about <span className="font-semibold">{size(preview.bytes)}</span>. Deleting them is permanent and cannot be undone.
              </p>
              <div className="mt-2.5 flex flex-wrap gap-2">
                <button type="button" className={BTN_DANGER} onClick={() => void run(true)} disabled={busy}>
                  {busy ? <Shimmer>Deleting</Shimmer> : "Delete permanently"}
                </button>
                <button type="button" className={BTN_SECONDARY} onClick={() => setPreview(null)} disabled={busy}>
                  Keep them
                </button>
              </div>
            </>
          )}
        </div>
      )}
      {note && <p className="mt-2 text-w-xs text-w-muted">{note}</p>}
    </SettingsRow>
  );
}

export function StorageSection() {
  const [report, setReport] = useState<Report | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    fetch("/api/world/storage", { cache: "no-store" })
      .then(async (r) => {
        if (!r.ok) throw new Error(await readError(r));
        setReport((await r.json()) as Report);
        setError(null);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : "Could not measure your storage."));
  }, []);
  useEffect(load, [load]);
  const names = new Map((report?.agents ?? []).map((a) => [a.agent_id, a.name]));
  return (
    <div className="space-y-8">
      <SettingsGroup
        title="What your agents keep"
        description={report ? `About ${size(report.total_bytes)} across ${report.agents.length} agents in this organization.` : "Measuring your agents' chats, runs and activity."}
      >
        {error && <InfoNote>{error}</InfoNote>}
        {!report && !error && (
          <p className="px-1 text-w-sm text-w-muted">
            <Shimmer>Measuring</Shimmer>
          </p>
        )}
        {report && report.agents.length === 0 && <EmptyState icon={Database} title="No agents yet" description="Once your agents chat and work, what they keep shows up here." />}
        {report && report.agents.length > 0 && (
          <SettingsCard rows>
            {report.agents.map((a) => (
              <div key={a.agent_id} className="flex flex-wrap items-baseline gap-x-4 gap-y-1 px-4 py-3">
                <span className="min-w-0 flex-1 truncate text-w-sm font-medium text-w-text">{a.name}</span>
                <span className="text-w-sm font-semibold tabular-nums text-w-strong">{size(a.total_bytes)}</span>
                <span className="basis-full text-w-xs leading-relaxed text-w-muted">
                  Chats {size(a.message_bytes)} ({a.messages.toLocaleString()} messages), runs {size(a.run_bytes)} ({a.runs.toLocaleString()}), step logs {size(a.step_bytes)}, activity {size(a.event_bytes)}.{" "}
                  {a.last_active ? (
                    <>
                      Last active <RelativeTime at={a.last_active} />.
                    </>
                  ) : (
                    "Never active."
                  )}
                </span>
              </div>
            ))}
          </SettingsCard>
        )}
      </SettingsGroup>

      <SettingsGroup title="Clean up" description="Loka never deletes anything on its own. Pick what to remove and how old, check what would go, then confirm.">
        <SettingsCard rows>
          {KINDS.map((k) => (
            <Cleanup key={k.kind} {...k} agents={report?.agents ?? []} onDone={load} />
          ))}
        </SettingsCard>
        <InfoNote>Your main agent can do this too: ask it what is using space. It always shows you what would go and waits for your yes before deleting.</InfoNote>
      </SettingsGroup>

      {report && report.cleanups.length > 0 && (
        <SettingsGroup title="Past cleanups">
          <SettingsCard rows>
            {report.cleanups.map((c) => (
              <div key={`${c.created_at}-${c.kind}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-3">
                <Broom size={16} weight="duotone" className="shrink-0 text-w-muted" />
                <span className="min-w-0 flex-1 text-w-sm text-w-text">
                  {LABEL[c.kind] ?? c.kind} older than {c.older_than} days{c.agent_id ? ` for ${names.get(c.agent_id) ?? "one agent"}` : ""}: {c.rows_removed.toLocaleString()} items, {size(c.bytes_freed)}
                  {c.by_agent_id ? `, by ${names.get(c.by_agent_id) ?? "your main agent"}` : ""}
                </span>
                <RelativeTime at={c.created_at} className="text-w-2xs text-w-muted" />
              </div>
            ))}
          </SettingsCard>
        </SettingsGroup>
      )}
    </div>
  );
}
