"use client";

import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Terminal, Trash } from "@phosphor-icons/react";
import { qk } from "@/lib/query-keys";
import { cn } from "@/lib/utils";
import { worldApi, type CommandRule, type WorldAgent } from "@/lib/world";
import { WSelect } from "./controls";
import { EmptyState, InfoNote } from "./settings-shell";
import { BTN_PRIMARY_SM, INPUT } from "./styles";

const EMPTY_AGENTS: WorldAgent[] = [];
const EVERYONE = "all";

const KIND_LABEL: Record<CommandRule["kind"], string> = { allow: "Allow", deny: "Block" };
const KIND_STYLE: Record<CommandRule["kind"], string> = { allow: "bg-w-mint text-w-ok", deny: "bg-w-coral text-w-strong" };

export function CommandRulesPanel({ ownerId }: { ownerId: string }) {
  const queryClient = useQueryClient();
  const rulesQuery = useQuery({ queryKey: qk.commandRules(ownerId), queryFn: () => worldApi.commandRules() });
  const agents: WorldAgent[] = useQuery({ queryKey: qk.agents(), queryFn: () => worldApi.agents() }).data ?? EMPTY_AGENTS;
  const ownAgents = useMemo(() => agents.filter((agent) => agent.owner_user_id === ownerId), [agents, ownerId]);
  const [kind, setKind] = useState<CommandRule["kind"]>("deny");
  const [scope, setScope] = useState(EVERYONE);
  const [rule, setRule] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rules = rulesQuery.data?.rules ?? [];
  const nameOf = (id: string | null) => (id ? ownAgents.find((agent) => agent.id === id)?.name ?? "An agent" : "All your agents");
  const allowOnly = new Set(rules.filter((r) => r.kind === "allow").map((r) => r.agent_id ?? EVERYONE));

  const refresh = () => queryClient.invalidateQueries({ queryKey: qk.commandRules(ownerId) });

  const add = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!rule.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await worldApi.addCommandRule({ kind, rule: rule.trim(), agent_id: scope === EVERYONE ? null : scope });
      setRule("");
      await refresh();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "Could not add that rule.");
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    setBusy(true);
    setError(null);
    try {
      await worldApi.deleteCommandRule(id);
      await refresh();
    } catch {
      setError("Could not remove that rule.");
    } finally {
      setBusy(false);
    }
  };

  const groups = [EVERYONE, ...ownAgents.map((agent) => agent.id)]
    .map((id) => ({ id, rules: rules.filter((r) => (r.agent_id ?? EVERYONE) === id) }))
    .filter((group) => group.rules.length > 0);

  return (
    <div className="flex flex-col gap-3">
      <p className="text-w-xs leading-relaxed text-w-text2">
        Off by default, so agents run commands the way their CLI allows. A block rule stops a command everywhere it appears, even inside pipes, <span className="font-mono">$( )</span>, <span className="font-mono">eval</span> or <span className="font-mono">sudo</span>. Adding any allow rule switches that agent to allow only: it may run the listed commands and harmless shell basics like <span className="font-mono">cd</span> and <span className="font-mono">echo</span>, nothing else. A rule is a command and its first words, like <span className="font-mono">git push</span> or <span className="font-mono">npm test</span>.
      </p>
      <form onSubmit={add} className="flex flex-wrap items-center gap-2 rounded-lg border-2 border-w-line2 bg-w-card p-3">
        <WSelect
          size="sm"
          className="w-28"
          ariaLabel="Rule kind"
          value={kind}
          onChange={(value) => setKind(value as CommandRule["kind"])}
          options={[
            { value: "deny", label: "Block" },
            { value: "allow", label: "Allow" },
          ]}
        />
        <input
          className={cn(INPUT, "min-w-40 flex-1 font-mono text-w-sm")}
          aria-label="Command"
          placeholder="git push"
          autoComplete="off"
          spellCheck={false}
          maxLength={200}
          value={rule}
          onChange={(event) => setRule(event.target.value)}
        />
        <WSelect
          size="sm"
          className="w-44"
          ariaLabel="Which agents"
          value={scope}
          onChange={setScope}
          options={[{ value: EVERYONE, label: "All your agents" }, ...ownAgents.map((agent) => ({ value: agent.id, label: agent.name }))]}
        />
        <button type="submit" disabled={busy || !rule.trim()} className={BTN_PRIMARY_SM}>
          Add rule
        </button>
        {kind === "allow" && !allowOnly.has(scope) && (
          <p className="w-full text-w-2xs text-w-warn">
            This is the first allow rule for {scope === EVERYONE ? "your agents" : nameOf(scope)}, so they will only be able to run allowed commands from their next run.
          </p>
        )}
      </form>
      {rules.length === 0 && rulesQuery.isSuccess && (
        <EmptyState icon={Terminal} title="No command rules" description="Agents run commands the way their CLI allows. Add a rule above or ask your main agent to." />
      )}
      {groups.map((group) => (
        <div key={group.id} className="rounded-lg border-2 border-w-line bg-w-field/70 p-3">
          <p className="mb-2 text-w-sm font-semibold text-w-strong">
            {nameOf(group.id === EVERYONE ? null : group.id)}
            {allowOnly.has(group.id) && <span className="ml-2 rounded-md bg-w-lemon px-1.5 py-0.5 text-w-2xs font-normal text-w-text2">allow only</span>}
          </p>
          <ul className="flex flex-col gap-1.5">
            {group.rules.map((r) => (
              <li key={r.id} className="flex items-center gap-2 text-w-xs">
                <span className={cn("rounded-md px-1.5 py-0.5 text-w-2xs", KIND_STYLE[r.kind])}>{KIND_LABEL[r.kind]}</span>
                <span className="min-w-0 flex-1 truncate font-mono">{r.rule}</span>
                {r.created_by !== "person" && <span className="text-w-2xs text-w-muted">added by {r.created_by.replace(/^agent:/, "")}</span>}
                <button type="button" disabled={busy} onClick={() => void remove(r.id)} title="Remove this rule" aria-label={`Remove ${r.rule}`} className="text-w-muted hover:text-w-strong">
                  <Trash size={13} weight="bold" />
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
      <InfoNote>
        Rules are checked for Claude Code quests, and for OpenCode quests outside the sandbox. Codex, Cursor and oh-my-pi run commands their own way, so set their limits in their own settings. An allowed command that runs other programs (like bash, python or make) can still run anything, so leave those off an allow list when you mean it.
      </InfoNote>
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </div>
  );
}
