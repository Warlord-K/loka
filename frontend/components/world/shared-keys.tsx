"use client";

import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { qk } from "@/lib/query-keys";
import { Trash, ShareNetwork } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type SharedKeysResponse, type WorldAgent } from "@/lib/world";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, CHUNKY, INPUT } from "./styles";
import { EmptyState, InfoNote } from "./settings-shell";

const EMPTY_AGENTS: WorldAgent[] = [];

export function SharedKeysPanel({ embedded = false }: { embedded?: boolean } = {}) {
  const queryClient = useQueryClient();
  const keysQuery = useQuery({ queryKey: qk.sharedKeys(), queryFn: () => worldApi.sharedKeys() });
  const data: SharedKeysResponse | undefined = keysQuery.data;
  const agents: WorldAgent[] = useQuery({ queryKey: qk.agents(), queryFn: () => worldApi.agents() }).data ?? EMPTY_AGENTS;
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [note, setNote] = useState("");
  const [replacing, setReplacing] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [granting, setGranting] = useState<string | null>(null);
  const [deleteArmed, setDeleteArmed] = useState<string | null>(null);
  const [actionError, setError] = useState<string | null>(null);
  const loadError = data
    ? data.configured
      ? null
      : "Secure environment storage is not configured by the operator."
    : keysQuery.isError
      ? "Could not load shared keys."
      : null;
  const error = actionError ?? loadError;

  const refresh = async () => {
    setError(null);
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: qk.sharedKeys() }),
      queryClient.invalidateQueries({ queryKey: qk.agents() }),
    ]);
  };

  useEffect(() => () => setValue(""), []);

  const sorted = useMemo(
    () => [...agents].filter((a) => a.owner_user_id).sort((a, b) => Number(a.visibility === "private") - Number(b.visibility === "private") || a.name.localeCompare(b.name)),
    [agents],
  );

  const clearDraft = () => {
    setName("");
    setValue("");
    setNote("");
    setReplacing(null);
  };

  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    const keyName = name.trim();
    if (!keyName || !data?.configured) return;
    if (!replacing && !value) return;
    if (!replacing && data.keys.some((k) => k.name === keyName)) {
      setError(`Use Replace beside ${keyName} to change it.`);
      return;
    }
    setPending(true);
    setError(null);
    try {
      await worldApi.saveSharedKey(keyName, { value: value || undefined, note });
      clearDraft();
      await refresh();
    } catch {
      setError("Could not save that key. Check the name and try again.");
    } finally {
      setPending(false);
    }
  };

  const setGrant = async (keyName: string, agentId: string, enabled: boolean) => {
    const key = data?.keys.find((k) => k.name === keyName);
    if (!key) return;
    const ids = enabled ? Array.from(new Set([...key.agent_ids, agentId])) : key.agent_ids.filter((id) => id !== agentId);
    setGranting(keyName);
    setError(null);
    try {
      await worldApi.setSharedKeyAgents(keyName, ids);
      await refresh();
    } catch {
      setError("Could not change who can use that key.");
    } finally {
      setGranting(null);
    }
  };

  const remove = async (keyName: string) => {
    if (deleteArmed !== keyName) {
      setDeleteArmed(keyName);
      return;
    }
    setPending(true);
    setError(null);
    try {
      await worldApi.deleteSharedKey(keyName);
      if (replacing === keyName) clearDraft();
      setDeleteArmed(null);
      await refresh();
    } catch {
      setError("Could not delete that key.");
    } finally {
      setPending(false);
    }
  };

  const configured = data?.configured === true;

  return (
    <section id="world-shared-keys" tabIndex={-1} className={cn("scroll-mt-5 outline-none", !embedded && "rounded-xl border-2 border-w-line bg-w-field/50 p-4 focus-visible:ring-2 focus-visible:ring-w-teal")}>
      {!embedded && (
        <>
          <h3 className="mb-3 text-w-sm font-semibold text-w-strong">Shared keys</h3>
          <p className="mb-3 text-w-xs leading-relaxed text-w-text2">
            World keys that admins set for team agents, like a public bot that needs a Linear or GitHub token. Nobody can read a value back, not even admins. Pick the agents that may use each one; an agent&apos;s own key with the same name wins.
          </p>
        </>
      )}
      {!data && !error && <p className="text-w-sm text-w-muted">Loading...</p>}
      <form onSubmit={save} className="mb-4 flex flex-col gap-2 rounded-lg border-2 border-w-line2 bg-w-card p-3">
        <p className="text-w-sm font-semibold text-w-strong">{replacing ? `Change ${replacing}` : "Add a shared key"}</p>
        <input
          className={cn(INPUT, "font-mono text-w-sm")}
          aria-label="Shared key name"
          autoComplete="off"
          spellCheck={false}
          placeholder="LINEAR_API_KEY"
          maxLength={128}
          value={name}
          disabled={!configured || pending || replacing !== null}
          onChange={(event) => setName(event.target.value.toUpperCase())}
        />
        <input
          className={cn(INPUT, "font-mono text-w-sm")}
          type="password"
          aria-label="Shared key value"
          autoComplete="new-password"
          placeholder={replacing ? "New value (leave empty to keep it)" : "Value"}
          maxLength={65536}
          value={value}
          disabled={!configured || pending}
          onChange={(event) => setValue(event.target.value)}
        />
        <input
          className={cn(INPUT, "text-w-sm")}
          aria-label="What it is for"
          placeholder="What it is for, optional"
          maxLength={500}
          value={note}
          disabled={!configured || pending}
          onChange={(event) => setNote(event.target.value)}
        />
        <div className="flex items-center justify-end gap-2">
          {(name || value || note || replacing) && (
            <button type="button" onClick={clearDraft} disabled={pending} className={BTN_SECONDARY_SM}>Cancel</button>
          )}
          <button type="submit" disabled={!configured || pending || !name.trim() || (!replacing && !value)} className={BTN_PRIMARY_SM}>
            {pending ? "Saving..." : replacing ? "Save changes" : "Save key"}
          </button>
        </div>
      </form>
      <ul className="flex flex-col gap-2">
        {data?.keys.length === 0 && configured && (
          <li>
            <EmptyState icon={ShareNetwork} title="No shared keys" description="Add one above, then pick the agents that may use it." />
          </li>
        )}
        {data?.keys.map((key) => (
          <li key={key.name} className="rounded-lg border-2 border-w-line bg-w-field/70 p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="min-w-0 flex-1">
                <b className="block truncate font-mono text-w-sm">{key.name}</b>
                <span className="text-w-2xs text-w-muted">
                  {key.note ? `${key.note} · ` : ""}Updated {new Date(key.updated_at).toLocaleString()}
                </span>
              </span>
              <button
                type="button"
                disabled={!configured || pending}
                onClick={() => {
                  setReplacing(key.name);
                  setName(key.name);
                  setNote(key.note);
                  setValue("");
                  setError(null);
                }}
                className={BTN_SECONDARY_SM}
              >
                Replace
              </button>
              <button
                type="button"
                disabled={pending}
                onClick={() => void remove(key.name)}
                onBlur={() => setDeleteArmed(null)}
                className={cn(CHUNKY, "flex items-center gap-1 px-2.5 py-1 text-w-xs", deleteArmed === key.name ? "bg-w-alert text-w-onalert" : "bg-w-card")}
              >
                <Trash size={13} weight="bold" />
                {deleteArmed === key.name ? "Really delete?" : "Delete"}
              </button>
            </div>
            <div className="mt-3 flex flex-wrap gap-x-3 gap-y-1.5">
              {sorted.length === 0 && <span className="text-w-xs text-w-muted">No agents yet.</span>}
              {sorted.map((agent) => (
                <label key={agent.id} className="flex items-center gap-1.5 text-w-xs">
                  <input
                    type="checkbox"
                    className="h-4 w-4 accent-w-teal"
                    checked={key.agent_ids.includes(agent.id)}
                    disabled={granting === key.name}
                    onChange={(event) => void setGrant(key.name, agent.id, event.target.checked)}
                  />
                  {agent.name}
                  {agent.visibility !== "private" && <span className="rounded-md bg-w-lemon px-1 text-w-2xs text-w-text2">public</span>}
                </label>
              ))}
            </div>
          </li>
        ))}
      </ul>
      <InfoNote className="mt-3">Handing an agent to someone else revokes its shared keys. A process that is already running may keep the value.</InfoNote>
      {error && <p role="alert" className="mt-3 text-w-xs text-w-danger">{error}</p>}
    </section>
  );
}
