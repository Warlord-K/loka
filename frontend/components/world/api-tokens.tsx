"use client";

import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { qk } from "@/lib/query-keys";
import { Copy, Check } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type ApiToken, type CreatedApiToken, type WorldAgent } from "@/lib/world";
import { ConfirmDelete, WSelect } from "./controls";
import { BTN_PRIMARY, CHUNKY, INPUT } from "./styles";

function mcpUrl(): string {
  if (process.env.NEXT_PUBLIC_MCP_URL) return process.env.NEXT_PUBLIC_MCP_URL;
  if (typeof window === "undefined") return "/mcp";
  if (window.location.hostname === "loka.ygpt.dev") return "https://api.world.ygpt.dev/mcp";
  return `${window.location.origin}/mcp`;
}

function day(at: string): string {
  return new Date(at).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function when(at: string | null): string {
  if (!at) return "never";
  return new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={async () => {
        await navigator.clipboard.writeText(text).catch(() => {});
        setDone(true);
        window.setTimeout(() => setDone(false), 1500);
      }}
      className={cn(CHUNKY, "grid h-8 w-8 shrink-0 place-items-center bg-w-card hover:bg-w-sand")}
    >
      {done ? <Check size={14} weight="bold" /> : <Copy size={14} weight="bold" />}
    </button>
  );
}

const SCOPES = [
  { value: "write", label: "Read and write" },
  { value: "read", label: "Read only" },
];

const LIFETIMES = [
  { value: "30", label: "Expires in 30 days" },
  { value: "90", label: "Expires in 90 days" },
  { value: "365", label: "Expires in a year" },
];

const EMPTY_TOKENS: ApiToken[] = [];
const EMPTY_AGENTS: WorldAgent[] = [];

export function ApiTokensSection({ canWrite }: { canWrite: boolean }) {
  const queryClient = useQueryClient();
  const tokens: ApiToken[] = useQuery({ queryKey: qk.tokens(), queryFn: () => worldApi.apiTokens() }).data ?? EMPTY_TOKENS;
  const agents: WorldAgent[] = useQuery({ queryKey: qk.agents(), queryFn: () => worldApi.agents() }).data ?? EMPTY_AGENTS;
  const [name, setName] = useState("");
  const [scope, setScope] = useState<"read" | "write">(canWrite ? "write" : "read");
  const [agent, setAgent] = useState("");
  const [days, setDays] = useState("90");
  const [created, setCreated] = useState<CreatedApiToken | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const refresh = () => void queryClient.invalidateQueries({ queryKey: qk.tokens() });

  const agentOptions = useMemo(
    () => [{ value: "", label: "Every agent I can reach" }, ...agents.map((a) => ({ value: a.id, label: `Only ${a.name}` }))],
    [agents],
  );

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    setNote(null);
    try {
      const token = await worldApi.createApiToken(name.trim(), scope, agent ? [agent] : null, Number(days));
      setCreated(token);
      setName("");
      refresh();
    } catch (err) {
      setNote(err instanceof Error ? err.message : "Could not create the token.");
    }
  };

  const revoke = async (id: string) => {
    try {
      await worldApi.revokeApiToken(id);
      if (created?.id === id) setCreated(null);
      refresh();
    } catch {
      setNote("Could not revoke that token. Try again.");
    }
  };

  const url = mcpUrl();
  const snippet = `claude mcp add --transport http world ${url} --header "Authorization: Bearer ${created?.secret ?? "wld_..."}"`;
  const active = tokens.filter((t) => !t.revoked_at && new Date(t.expires_at) > new Date());

  return (
    <div className="space-y-3 text-w-base">
      <p className="text-w-xs leading-relaxed text-w-muted">
        Let Claude Code, Cursor or any MCP client talk to your agents, start quests and follow them. A token acts as you, with your access.
      </p>
      <form onSubmit={create} className="space-y-2">
        <input className={cn(INPUT, "w-full")} value={name} maxLength={60} placeholder="Name, like laptop or cursor" onChange={(e) => setName(e.target.value)} aria-label="Token name" />
        <div className="flex flex-wrap gap-2">
          {canWrite && <WSelect value={scope} onChange={(v) => setScope(v as "read" | "write")} options={SCOPES} ariaLabel="Token access" size="sm" />}
          <WSelect value={agent} onChange={setAgent} options={agentOptions} ariaLabel="Agents this token can reach" size="sm" />
          <WSelect value={days} onChange={setDays} options={LIFETIMES} ariaLabel="Token lifetime" size="sm" />
          <button type="submit" disabled={!name.trim()} className={BTN_PRIMARY}>
            Create token
          </button>
        </div>
      </form>
      {note && <p className="text-w-xs text-w-text2">{note}</p>}
      {created && (
        <div className="space-y-2 rounded-lg border-2 border-w-ink bg-w-mint/40 p-3">
          <p className="text-w-xs">Copy it now. Loka only shows a token once.</p>
          <div className="flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded-md bg-w-field px-2 py-1 font-mono text-w-xs">{created.secret}</code>
            <CopyButton text={created.secret} label="Copy token" />
          </div>
        </div>
      )}
      <div>
        <div className="mb-1 text-w-xs font-medium text-w-muted">Connect</div>
        <div className="flex items-start gap-2">
          <code className="min-w-0 flex-1 whitespace-pre-wrap break-all rounded-md bg-w-field px-2 py-1 font-mono text-w-xs leading-relaxed">{snippet}</code>
          <CopyButton text={snippet} label="Copy connect command" />
        </div>
        <p className="mt-1 text-w-xs text-w-muted">Other clients: streamable HTTP at {url} with the same Authorization header.</p>
      </div>
      {active.length > 0 && (
        <ul className="space-y-1">
          {active.map((t) => (
            <li key={t.id} className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate">
                <b>{t.name}</b> <span className="font-mono text-w-xs text-w-muted">{t.prefix}...</span>
                <span className="block text-w-xs text-w-muted">
                  {t.scope === "write" ? "Read and write" : "Read only"}
                  {t.agent_ids ? `, ${t.agent_ids.length === 1 ? agents.find((a) => a.id === t.agent_ids?.[0])?.name ?? "1 agent" : `${t.agent_ids.length} agents`}` : ""}
                  , expires {day(t.expires_at)}, last used {when(t.last_used_at)}
                </span>
              </span>
              <ConfirmDelete label={`Revoke ${t.name}`} onConfirm={() => void revoke(t.id)} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
