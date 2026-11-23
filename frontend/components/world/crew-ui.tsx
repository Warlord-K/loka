"use client";

import { useId } from "react";
import { useQuery } from "@tanstack/react-query";
import { Terminal } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { modelName, runtimeName } from "@/lib/model-names";
import { qk } from "@/lib/query-keys";
import { cliOptions, worldApi, type BackendModels, type WorldAgent } from "@/lib/world";
import { mixProblem, resolveMix, slotRuntime, type MixSlot } from "@/lib/quest-board";
import { roomApi } from "@/lib/quest-room-api";
import { WCombobox, WSelect } from "./controls";
import { PILL_NEUTRAL } from "./styles";
import { Shimmer } from "./shimmer";

export function RuntimeBadge({ cli, model, className }: { cli: string | null | undefined; model: string | null | undefined; className?: string }) {
  const label = runtimeName(cli, model);
  if (!label) return null;
  return (
    <span title={`Runs on ${label}`} className={cn(PILL_NEUTRAL, "min-w-0 max-w-[12rem]", className)}>
      <Terminal size={10} weight="bold" />
      <span className="truncate">{label}</span>
    </span>
  );
}

const EMPTY_BACKENDS: Record<string, BackendModels> = {};

export function CrewMix({
  size,
  value,
  agent,
  standing = [],
  onChange,
  disabled,
}: {
  size: number;
  value: MixSlot[];
  agent: Pick<WorldAgent, "id" | "cli" | "model" | "runner_id">;
  standing?: MixSlot[];
  onChange: (next: MixSlot[]) => void;
  disabled?: boolean;
}) {
  const problemId = useId();
  const backends: Record<string, BackendModels> = useQuery({ queryKey: qk.backends(), queryFn: () => worldApi.backends() }).data ?? EMPTY_BACKENDS;
  const preview = useQuery({
    queryKey: qk.agent.crewPreview(agent.id, agent.runner_id, agent.cli, agent.model),
    queryFn: ({ signal }) => roomApi.crewPreview(agent.id, signal),
  });
  const lead = { cli: agent.cli, model: agent.model };
  const rows = Array.from({ length: size }, (_, i): MixSlot => value[i] ?? null);
  const clis = cliOptions("opencode" in backends, rows.find((row) => slotRuntime(row, lead)?.cli === "opencode") ? "opencode" : lead.cli);
  const defaults = resolveMix(size, lead, [], standing, preview.isSuccess ? preview.data.defaults : []);
  const set = (i: number, next: MixSlot) => onChange(rows.map((row, j) => (j === i ? next : row)));
  const problem = mixProblem(rows, size);
  return (
    <div>
    <ol className="space-y-1.5" aria-label="Helpers and what they run on" aria-invalid={!!problem} aria-describedby={problem ? problemId : undefined}>
      {rows.map((row, i) => {
        const picked = slotRuntime(row, lead);
        const cli = picked?.cli ?? "";
        const models = cli ? Array.from(new Set([...(backends[cli]?.models ?? []), ...(cli === "claude" ? ["opus", "sonnet", "haiku"] : [])])) : [];
        const fallback = defaults[i];
        return (
          <li key={i} className="flex flex-wrap items-center gap-2 text-w-xs text-w-muted">
            <span className="w-16 shrink-0 font-semibold text-w-text2">Helper {i + 1}</span>
            <WSelect
              size="sm"
              ariaLabel={`CLI for helper ${i + 1}`}
              className="w-44"
              disabled={disabled}
              value={cli}
              onChange={(v) => set(i, !v ? null : v === lead.cli ? { cli: v, model: lead.model } : { cli: v, model: backends[v]?.default_model || backends[v]?.models[0] || "" })}
              options={[
                { value: "", label: fallback.source === "agent" ? "Agent's default" : "Loka picks", hint: fallback.runtime ? runtimeName(fallback.runtime.cli, fallback.runtime.model) : undefined },
                ...clis.map((c) => ({ ...c, hint: c.value === lead.cli ? "the lead's" : undefined })),
              ]}
            />
            {picked ? (
              <div className="min-w-40 flex-1">
                <WCombobox
                  ariaLabel={`Model for helper ${i + 1}`}
                  value={picked.model}
                  onChange={(v) => set(i, { cli: picked.cli, model: v })}
                  placeholder="pick a model"
                  options={models.map((m) => ({ value: m, label: modelName(m), hint: picked.cli === lead.cli && m === lead.model ? "the lead's" : backends[picked.cli]?.labels?.[m] }))}
                  className="h-8 rounded-md px-2 text-w-xs"
                />
              </div>
            ) : (
              <span className="min-w-40 flex-1 truncate">
                {fallback.runtime ? <>Runs on <RuntimeBadge cli={fallback.runtime.cli} model={fallback.runtime.model} /></> : preview.isPending ? <Shimmer>Checking Loka&apos;s pick</Shimmer> : "Could not preview Loka's pick."}
              </span>
            )}
          </li>
        );
      })}
    </ol>
    {problem && (
      <p id={problemId} role="alert" className="mt-1.5 text-w-xs text-w-danger">
        {problem}
      </p>
    )}
    </div>
  );
}
