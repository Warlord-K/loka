"use client";

import { SOLO } from "@/lib/edition";
import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { qk } from "@/lib/query-keys";
import { Trash, Key, FileArrowUp } from "@phosphor-icons/react";
import { AnimatePresence } from "framer-motion";
import { cn } from "@/lib/utils";
import {
  worldApi,
  type EnvironmentChoice,
  type EnvironmentEntry,
  type EnvironmentResponse,
  type EnvironmentVariant,
  type ProviderCheck,
  type ProviderKey,
  type WorldAgent,
} from "@/lib/world";
import { WSelect } from "./controls";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, CHUNKY, INPUT, PILL_INFO } from "./styles";
import { refreshReadiness } from "./setup-hint";
import { Shimmer } from "./work-ui";
import { EmptyState, InfoNote } from "./settings-shell";
import { EnvImportDialog } from "./env-import";

const STATUS_LABEL: Record<ProviderKey["status"], string> = {
  own: "Your key",
  shared: "Using the shared server key",
  missing: "Not set",
};

const STATUS_STYLE: Record<ProviderKey["status"], string> = {
  own: "bg-w-mint text-w-ok",
  shared: "bg-w-aqua text-w-link",
  missing: "bg-w-card text-w-muted",
};

const EMPTY_AGENTS: WorldAgent[] = [];

export function EnvironmentPanel({ ownerId, embedded = false }: { ownerId: string; embedded?: boolean }) {
  const queryClient = useQueryClient();
  const environmentQuery = useQuery({ queryKey: qk.environment(ownerId), queryFn: () => worldApi.environment() });
  const environment: EnvironmentResponse | undefined = environmentQuery.data;
  const agents: WorldAgent[] = useQuery({ queryKey: qk.agents(), queryFn: () => worldApi.agents() }).data ?? EMPTY_AGENTS;
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [replacing, setReplacing] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [importing, setImporting] = useState(false);
  const [granting, setGranting] = useState<string | null>(null);
  const [deleteArmed, setDeleteArmed] = useState<string | null>(null);
  const [actionError, setError] = useState<string | null>(null);
  const [warning, setWarning] = useState<string | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [checks, setChecks] = useState<Record<string, ProviderCheck>>({});
  const valueRef = useRef<HTMLInputElement>(null);

  const loadError = environment
    ? environment.configured
      ? null
      : "Secure environment storage is not configured by the operator."
    : environmentQuery.isError
      ? "Could not load your environment."
      : null;
  const error = actionError ?? loadError;

  const refresh = async () => {
    setError(null);
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: qk.environment(ownerId) }),
      queryClient.invalidateQueries({ queryKey: qk.agents() }),
    ]);
  };

  useEffect(() => () => setValue(""), []);

  const ownAgents = useMemo(() => agents.filter((agent) => agent.owner_user_id === ownerId), [agents, ownerId]);

  const clearDraft = () => {
    setName("");
    setValue("");
    setReplacing(null);
    setWarning(null);
  };

  const providerFor = (envName: string) => environment?.providers?.find((provider) => provider.env === envName);

  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    const entryName = name.trim();
    if (!entryName || !value || !environment?.configured) return;
    if (environment.entries.some((entry) => entry.name === entryName) && replacing !== entryName) {
      setError(`Use Replace beside ${entryName} to replace its value.`);
      return;
    }
    setPending(true);
    setError(null);
    try {
      const provider = providerFor(entryName);
      if (provider && warning === null && !value.trim().startsWith("op://")) {
        const check = await worldApi.checkProviderKey(provider.id, { value }).catch(() => null);
        if (check?.problem) {
          setWarning(check.problem);
          return;
        }
      }
      await worldApi.saveEnvironment(entryName, value);
      const wanting = (environment.wanted ?? []).filter((w) => w.keys.includes(entryName)).map((w) => w.agent_id);
      if (wanting.length) {
        const existing = environment.entries.find((entry) => entry.name === entryName)?.agent_ids ?? [];
        await worldApi.setEnvironmentAgents(entryName, Array.from(new Set([...existing, ...wanting])));
      }
      clearDraft();
      await refresh();
      refreshReadiness();
    } catch {
      setError("Could not save that value. Check the name and try again.");
    } finally {
      setPending(false);
    }
  };

  const importKeys = async (items: { name: string; value: string }[]) => {
    if (!environment?.configured) return;
    let result: { saved: string[]; rejected: string[] };
    try {
      result = await worldApi.importEnvironment(items);
    } catch {
      throw new Error("Could not save those keys. Nothing was changed.");
    }
    for (const entryName of result.saved) {
      const wanting = (environment.wanted ?? []).filter((w) => w.keys.includes(entryName)).map((w) => w.agent_id);
      if (!wanting.length) continue;
      const existing = environment.entries.find((entry) => entry.name === entryName)?.agent_ids ?? [];
      await worldApi.setEnvironmentAgents(entryName, Array.from(new Set([...existing, ...wanting]))).catch(() => undefined);
    }
    await refresh();
    refreshReadiness();
    setError(result.rejected.length ? `Saved ${result.saved.length}. Could not save ${result.rejected.join(", ")}.` : null);
  };

  const setGrant = async (entryName: string, agentId: string, enabled: boolean) => {
    const entry = environment?.entries.find((candidate) => candidate.name === entryName);
    if (!entry) return;
    const agentIds = enabled
      ? Array.from(new Set([...entry.agent_ids, agentId]))
      : entry.agent_ids.filter((id) => id !== agentId);
    setGranting(entryName);
    setError(null);
    try {
      await worldApi.setEnvironmentAgents(entryName, agentIds);
      await refresh();
    } catch {
      setError("Could not change access to that value.");
    } finally {
      setGranting(null);
    }
  };

  const remove = async (entryName: string) => {
    if (deleteArmed !== entryName) {
      setDeleteArmed(entryName);
      return;
    }
    setPending(true);
    setError(null);
    try {
      await worldApi.deleteEnvironment(entryName);
      if (replacing === entryName) clearDraft();
      setDeleteArmed(null);
      await refresh();
    } catch {
      setError("Could not delete that value.");
    } finally {
      setPending(false);
    }
  };

  const configured = environment?.configured === true;

  const dismiss = async (agentId: string, keyName: string) => {
    setError(null);
    try {
      await worldApi.dismissWantedKey(agentId, keyName);
      await refresh();
    } catch {
      setError("Could not dismiss that.");
    }
  };

  const startAdding = (keyName: string) => {
    setReplacing(environment?.entries.some((entry) => entry.name === keyName) ? keyName : null);
    setName(keyName);
    setValue("");
    setError(null);
    setWarning(null);
    valueRef.current?.focus();
  };

  const testKey = async (provider: ProviderKey) => {
    setTesting(provider.id);
    try {
      const check = await worldApi.checkProviderKey(provider.id, { live: true });
      setChecks((current) => ({ ...current, [provider.id]: check }));
    } catch {
      setChecks((current) => ({ ...current, [provider.id]: { problem: null, live: "failed", detail: "Could not run the test." } }));
    } finally {
      setTesting(null);
    }
  };

  return (
    <section id="world-environment" tabIndex={-1} className={cn("scroll-mt-5 outline-none", !embedded && "rounded-xl border-2 border-w-line bg-w-field/50 p-4 focus-visible:ring-2 focus-visible:ring-w-teal")}>
      {!embedded && (
        <>
          <h3 className="mb-3 text-w-sm font-semibold text-w-strong">Environment</h3>
          <p className="mb-3 text-w-xs leading-relaxed text-w-text2">
            Store a value outside chat, then choose the agents that may use it while working. Values are never shown again.
          </p>
        </>
      )}
      {!environment && !error && <p className="text-w-sm text-w-muted">Loading...</p>}
      {environment && !configured && (
        <p className="mb-3 rounded-lg border-2 border-w-coral bg-w-card p-3 text-w-xs text-w-text2">
          Ask the operator to configure WORLD_SECRETS_KEY for the API and agent services before adding values.
        </p>
      )}
      {environment && environment.wanted.length > 0 && (
        <div className="mb-4 rounded-lg border-2 border-w-gold bg-w-lemon p-3">
          <p className="mb-1 text-w-sm font-semibold text-w-strong">Keys your agents need</p>
          <p className="mb-2 text-w-xs text-w-text2">These agents were handed to you. Their old keys stayed with their previous owner, so add your own values.</p>
          <ul className="flex flex-col gap-1.5">
            {environment.wanted.flatMap((w) =>
              w.keys.map((keyName) => {
                const exists = environment.entries.some((entry) => entry.name === keyName);
                return (
                  <li key={`${w.agent_id}:${keyName}`} className="flex flex-wrap items-center gap-2 text-w-xs">
                    <span className="min-w-0 flex-1">
                      {w.agent_name} needs <b className="font-mono">{keyName}</b>
                    </span>
                    {exists ? (
                      <button type="button" disabled={granting === keyName} onClick={() => void setGrant(keyName, w.agent_id, true)} className={BTN_PRIMARY_SM}>
                        Grant yours
                      </button>
                    ) : (
                      <button type="button" disabled={!configured} onClick={() => startAdding(keyName)} className={BTN_PRIMARY_SM}>
                        Add value
                      </button>
                    )}
                    <button type="button" onClick={() => void dismiss(w.agent_id, keyName)} className="text-w-xs text-w-muted underline hover:text-w-strong">
                      Not needed
                    </button>
                  </li>
                );
              }),
            )}
          </ul>
        </div>
      )}
      {environment?.providers && environment.providers.length > 0 && (
        <div className="mb-4 rounded-lg border-2 border-w-line2 bg-w-card p-3">
          <p className="mb-1 text-w-sm font-semibold text-w-strong">Provider keys</p>
          <p className="mb-2 text-w-xs text-w-text2">
            Your private agents use these automatically for features like live voice. Your own key always wins over the shared server key. Your Mac keeps using its own CLI logins.
          </p>
          <ul className="flex flex-col gap-2">
            {environment.providers.map((provider) => {
              const check = checks[provider.id];
              return (
                <li key={provider.id} className="flex flex-col gap-1 rounded-lg border-2 border-w-line bg-w-field/70 p-2.5">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="min-w-0 flex-1">
                      <b className="text-w-sm">{provider.label}</b> <span className="font-mono text-w-2xs text-w-muted">{provider.env}</span>
                    </span>
                    <span className={cn("rounded-md px-1.5 py-0.5 text-w-2xs", STATUS_STYLE[provider.status])}>{STATUS_LABEL[provider.status]}</span>
                    {provider.status === "own" && provider.testable && (
                      <button type="button" disabled={testing !== null} onClick={() => void testKey(provider)} className={BTN_SECONDARY_SM}>
                        {testing === provider.id ? <Shimmer>Testing</Shimmer> : "Test"}
                      </button>
                    )}
                    <button type="button" disabled={!configured || pending} onClick={() => startAdding(provider.env)} className={BTN_PRIMARY_SM}>
                      {provider.status === "own" ? "Replace" : "Add yours"}
                    </button>
                  </div>
                  <p className="text-w-xs text-w-text2">
                    {provider.status === "missing" ? `Add your ${provider.label} key to unlock: ` : "Unlocks: "}
                    {provider.unlocks}.{" "}
                    <a href={provider.docs} target="_blank" rel="noreferrer" className="text-w-link underline">
                      Get a key
                    </a>
                  </p>
                  {check && (
                    <p role="status" className={cn("text-w-xs", check.live === "ok" && !check.problem ? "text-w-ok" : "text-w-warn")}>
                      {check.live === "ok" ? "The key works." : check.detail ?? "The key did not pass the test."}
                      {check.problem ? ` ${check.problem}.` : ""}
                    </p>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
      )}
      <form onSubmit={save} className="mb-4 flex flex-col gap-2 rounded-lg border-2 border-w-line2 bg-w-card p-3">
        <div className="flex items-center justify-between gap-2">
          <p className="text-w-sm font-semibold text-w-strong">{replacing ? `Replace ${replacing}` : "Add a value"}</p>
          <button
            type="button"
            className={cn(BTN_SECONDARY_SM, "gap-1.5")}
            disabled={!environment?.configured}
            title="Paste a .env file to save many keys at once"
            onClick={() => setImporting(true)}
          >
            <FileArrowUp size={14} weight="bold" />
            Import
          </button>
        </div>
        <input
          className={cn(INPUT, "font-mono text-w-sm")}
          aria-label="Environment name"
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
          ref={valueRef}
          aria-label="Environment value"
          autoComplete="new-password"
          placeholder="Value"
          maxLength={65536}
          value={value}
          disabled={!configured || pending}
          onChange={(event) => {
            setValue(event.target.value);
            setWarning(null);
          }}
        />
        <p className="text-w-xs text-w-muted">
          Or paste a 1Password reference such as <span className="font-mono">op://Work/GitHub/token</span>. Your Mac reads it with op when a run starts, and Loka never stores the value.
        </p>
        {providerFor(name.trim()) && !warning && (
          <p className="text-w-xs text-w-muted">{providerFor(name.trim())?.label} keys usually {providerFor(name.trim())?.hint.replace(/^starts/, "start").replace(/^looks/, "look")}.</p>
        )}
        {warning && <p role="alert" className="text-w-xs text-w-warn">{warning}. Save again to keep it anyway.</p>}
        <div className="flex items-center justify-end gap-2">
          {(name || value || replacing) && (
            <button type="button" onClick={clearDraft} disabled={pending} className={BTN_SECONDARY_SM}>Cancel</button>
          )}
          <button type="submit" disabled={!configured || pending || !name.trim() || !value} className={BTN_PRIMARY_SM}>
            {pending ? "Saving..." : warning ? "Save anyway" : replacing ? "Replace value" : "Save value"}
          </button>
        </div>
      </form>
      <ul className="flex flex-col gap-2">
        {environment?.entries.length === 0 && configured && (
          <li>
            <EmptyState icon={Key} title="No saved keys" description="Add one above, then choose which agents can use it." />
          </li>
        )}
        {environment?.entries.map((entry) => (
          <li key={entry.name} className="rounded-lg border-2 border-w-line bg-w-field/70 p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="min-w-0 flex-1">
                <span className="flex min-w-0 items-center gap-2">
                  <b className="truncate font-mono text-w-sm">{entry.name}</b>
                  {entry.vault && <span className={PILL_INFO} title="Read from 1Password on your Mac when a run starts">Vault</span>}
                </span>
                <span className="text-w-2xs text-w-muted">Updated {new Date(entry.updated_at).toLocaleString()}</span>
              </span>
              <button
                type="button"
                disabled={!configured || pending}
                onClick={() => {
                  setReplacing(entry.name);
                  setName(entry.name);
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
                onClick={() => void remove(entry.name)}
                onBlur={() => setDeleteArmed(null)}
                className={cn(CHUNKY, "flex items-center gap-1 px-2.5 py-1 text-w-xs", deleteArmed === entry.name ? "bg-w-alert text-w-onalert" : "bg-w-card")}
              >
                <Trash size={13} weight="bold" />
                {deleteArmed === entry.name ? "Really delete?" : "Delete"}
              </button>
            </div>
            <p className="mb-2 mt-3 text-w-xs text-w-text2">Selected agents can read this value while working. Values are not added to chat.</p>
            <div className="flex flex-wrap gap-x-3 gap-y-1.5">
              {ownAgents.length === 0 && <span className="text-w-xs text-w-muted">Create an agent to grant access.</span>}
              {ownAgents.map((agent) => (
                <label key={agent.id} className="flex items-center gap-1.5 text-w-xs">
                  <input
                    type="checkbox"
                    className="h-4 w-4 accent-w-teal"
                    checked={entry.agent_ids.includes(agent.id)}
                    disabled={granting === entry.name}
                    onChange={(event) => void setGrant(entry.name, agent.id, event.target.checked)}
                  />
                  {agent.name}
                  {!SOLO && agent.visibility !== "private" && <span className="rounded-md bg-w-lemon px-1 text-w-2xs text-w-text2">public</span>}
                </label>
              ))}
            </div>
            {!SOLO && ownAgents.some((agent) => agent.visibility !== "private" && entry.agent_ids.includes(agent.id)) && (
              <p className="mt-2 text-w-2xs text-w-warn">Public agents work for anyone who talks to them, so others can put this key to use through them. Work other people ask for still waits for your approval, unless you marked them trusted.</p>
            )}
            <KeyEnvironments
              entry={entry}
              variants={(environment?.variants ?? []).filter((variant) => variant.name === entry.name)}
              choices={(environment?.choices ?? []).filter((choice) => choice.name === entry.name)}
              agents={
                providerFor(entry.name)
                  ? ownAgents.filter((agent) => agent.visibility === "private")
                  : ownAgents.filter((agent) => entry.agent_ids.includes(agent.id))
              }
              configured={configured}
              onChanged={refresh}
              onError={setError}
            />
            <InfoNote className="mt-3">Handing an agent to someone else revokes its access. A process that is already running may keep the value.</InfoNote>
          </li>
        ))}
      </ul>
      {error && <p role="alert" className="mt-3 text-w-xs text-w-danger">{error}</p>}
      <AnimatePresence>
        {importing && (
          <EnvImportDialog
            key="env-import"
            existing={(environment?.entries ?? []).map((entry) => entry.name)}
            onSave={importKeys}
            onClose={() => setImporting(false)}
          />
        )}
      </AnimatePresence>
    </section>
  );
}

function untilLabel(expires: string | null) {
  if (!expires) return null;
  const when = new Date(expires);
  return `until ${when.toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}`;
}

function KeyEnvironments({
  entry,
  variants,
  choices,
  agents,
  configured,
  onChanged,
  onError,
}: {
  entry: EnvironmentEntry;
  variants: EnvironmentVariant[];
  choices: EnvironmentChoice[];
  agents: WorldAgent[];
  configured: boolean;
  onChanged: () => Promise<void>;
  onError: (message: string | null) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [label, setLabel] = useState("");
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [armed, setArmed] = useState<string | null>(null);
  const cleanLabel = label.trim().toLowerCase().replace(/[\s_]+/g, "-");
  const labelOk = /^[a-z][a-z0-9-]{0,31}$/.test(cleanLabel) && cleanLabel !== "default";

  const close = () => {
    setAdding(false);
    setLabel("");
    setValue("");
  };

  const save = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!labelOk || !value) return;
    setBusy(true);
    onError(null);
    try {
      await worldApi.saveEnvironmentVariant(entry.name, cleanLabel, value);
      close();
      await onChanged();
    } catch (error) {
      onError(String(error).includes("409") ? "A key can have up to 12 environments." : "Could not save that value.");
    } finally {
      setBusy(false);
    }
  };

  const remove = async (environment: string) => {
    if (armed !== environment) {
      setArmed(environment);
      return;
    }
    setBusy(true);
    onError(null);
    try {
      await worldApi.deleteEnvironmentVariant(entry.name, environment);
      setArmed(null);
      await onChanged();
    } catch {
      onError("Could not delete that value.");
    } finally {
      setBusy(false);
    }
  };

  const choose = async (agentId: string, environment: string) => {
    setBusy(true);
    onError(null);
    try {
      await worldApi.chooseKeyEnvironment({ agent_id: agentId, name: entry.name, environment });
      await onChanged();
    } catch {
      onError("Could not switch that key.");
    } finally {
      setBusy(false);
    }
  };

  const options = [{ value: "default", label: "default" }, ...variants.map((variant) => ({ value: variant.environment, label: variant.environment }))];

  return (
    <div className="mt-3 rounded-lg border-2 border-w-line2 bg-w-card p-2.5">
      <div className="flex flex-wrap items-center gap-1.5">
        <span className="mr-1 text-w-xs font-semibold text-w-strong">Environments</span>
        <span className="rounded-md bg-w-field px-1.5 py-0.5 text-w-2xs text-w-text2">default</span>
        {variants.map((variant) => (
          <button
            key={variant.environment}
            type="button"
            disabled={busy}
            onClick={() => void remove(variant.environment)}
            onBlur={() => setArmed(null)}
            title={armed === variant.environment ? "Click again to delete" : `Delete the ${variant.environment} ${variant.vault ? "1Password reference" : "value"}`}
            className={cn(
              "flex items-center gap-1 rounded-md px-1.5 py-0.5 text-w-2xs",
              armed === variant.environment ? "bg-w-alert text-w-onalert" : "bg-w-aqua text-w-link hover:bg-w-field",
            )}
          >
            {variant.environment}
            {variant.vault && <span className="rounded bg-w-field px-1 font-semibold text-w-strong">Vault</span>}
            <Trash size={11} weight="bold" />
          </button>
        ))}
        {!adding && (
          <button type="button" disabled={!configured || busy} onClick={() => setAdding(true)} className="text-w-2xs text-w-link underline">
            Add another value
          </button>
        )}
      </div>
      {adding && (
        <form onSubmit={save} className="mt-2 flex flex-wrap items-center gap-2">
          <input
            className={cn(INPUT, "w-32 font-mono text-w-xs")}
            aria-label={`New environment for ${entry.name}`}
            placeholder="staging"
            autoComplete="off"
            spellCheck={false}
            maxLength={32}
            value={label}
            onChange={(event) => setLabel(event.target.value)}
          />
          <input
            className={cn(INPUT, "min-w-40 flex-1 font-mono text-w-xs")}
            type="password"
            aria-label={`Value of ${entry.name} in ${cleanLabel || "this environment"}`}
            placeholder="Value"
            autoComplete="new-password"
            maxLength={65536}
            value={value}
            onChange={(event) => setValue(event.target.value)}
          />
          <button type="button" onClick={close} disabled={busy} className={BTN_SECONDARY_SM}>
            Cancel
          </button>
          <button type="submit" disabled={busy || !labelOk || !value} className={BTN_PRIMARY_SM}>
            {busy ? "Saving..." : "Save"}
          </button>
          {label && !labelOk && <p className="w-full text-w-2xs text-w-warn">Use lowercase letters, digits and dashes, starting with a letter, and not the word default.</p>}
        </form>
      )}
      {variants.length > 0 && (
        <div className="mt-2.5 flex flex-col gap-1.5">
          {agents.length === 0 && <p className="text-w-2xs text-w-muted">No agent uses this key yet.</p>}
          {agents.map((agent) => {
            const choice = choices.find((candidate) => candidate.agent_id === agent.id);
            return (
              <div key={agent.id} className="flex flex-wrap items-center gap-2 text-w-xs">
                <span className="min-w-0 flex-1 truncate">{agent.name} uses</span>
                {choice && untilLabel(choice.expires_at) && <span className="text-w-2xs text-w-muted">{untilLabel(choice.expires_at)}</span>}
                <WSelect
                  size="sm"
                  className="w-36"
                  ariaLabel={`Which ${entry.name} value ${agent.name} uses`}
                  value={choice?.environment ?? "default"}
                  options={options}
                  disabled={busy}
                  onChange={(environment) => void choose(agent.id, environment)}
                />
              </div>
            );
          })}
          <p className="text-w-2xs text-w-muted">A switch applies from the agent&apos;s next run. Your main agent can switch keys for you too, for a while or for good.</p>
        </div>
      )}
    </div>
  );
}
