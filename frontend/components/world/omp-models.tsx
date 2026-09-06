"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Cpu, Key, X } from "@phosphor-icons/react";
import { AnimatePresence, motion } from "framer-motion";
import { MODAL_MOTION, SCRIM_MOTION } from "./motion";
import { cn } from "@/lib/utils";
import { worldApi, type OmpConfig, type WorldAgent } from "@/lib/world";
import { BTN, BTN_MD, BTN_PRIMARY, BTN_PRIMARY_SM, BTN_SECONDARY, CHUNKY, INPUT, MODAL, SCRIM } from "./styles";
import { CLOSE_BTN } from "./settings-shell";

const SECTION = "space-y-3 rounded-lg border-2 border-w-line2 bg-w-card p-3";

const EXAMPLE = `providers:
  my-llm:
    baseUrl: https://llm.example.com/v1
    api: openai-completions
    apiKey: \${secret:MY_LLM_KEY}
    models:
      - id: my-model
        name: My model
        contextWindow: 200000
        maxTokens: 32000`;

function pendingNames(status: OmpConfig, only?: string[]) {
  return status.secrets.filter((s) => !s.granted && (!only || only.includes(s.name)));
}

export function EquipSecretsDialog({
  agentId,
  agentName,
  status,
  only,
  onChanged,
  onClose,
}: {
  agentId: string;
  agentName: string;
  status: OmpConfig;
  only?: string[];
  onChanged: (status: OmpConfig) => void;
  onClose: () => void;
}) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const secrets = status.secrets.filter((s) => !only || only.includes(s.name));
  const ready = secrets.filter((s) => s.in_environment && !s.granted).map((s) => s.name);
  const blocked = !status.can_equip ? `Only ${agentName}'s owner can equip their keys.` : null;

  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      close.current();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const equip = async (names: string[]) => {
    if (!names.length) return;
    setBusy(true);
    setError(null);
    try {
      onChanged(await worldApi.equipOmpSecrets(agentId, names));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not equip those keys.");
    } finally {
      setBusy(false);
    }
  };

  const addAndEquip = async (name: string) => {
    const value = values[name];
    if (!value) return;
    setBusy(true);
    setError(null);
    try {
      await worldApi.saveEnvironment(name, value);
      setValues((v) => ({ ...v, [name]: "" }));
      onChanged(await worldApi.equipOmpSecrets(agentId, [name]));
    } catch (e) {
      setError(e instanceof Error && !/^\d+$/.test(e.message) ? e.message : `Could not save ${name}.`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <motion.div
      {...SCRIM_MOTION}
      className={cn(SCRIM, "z-50")}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <motion.div {...MODAL_MOTION} role="dialog" aria-modal="true" aria-label="Equip keys" className={cn(MODAL, "flex max-h-full max-w-md flex-col gap-3 p-4")}>
        <div className="flex items-center justify-between gap-2">
          <p className="flex items-center gap-2 text-w-sm font-semibold text-w-strong">
            <Key size={18} weight="bold" />
            Equip keys
          </p>
          <button type="button" onClick={onClose} aria-label="Close" className={CLOSE_BTN}>
            <X size={16} weight="bold" />
          </button>
        </div>
        <p className="text-w-sm text-w-text2">
          We found {secrets.length === 1 ? "this key" : "these keys"} in {agentName}&apos;s omp config: {secrets.map((s) => s.name).join(", ")}. Equip {secrets.length === 1 ? "it" : "them"} to {agentName}?
        </p>
        <ul className="space-y-1.5">
          {secrets.map((s) => (
            <li key={s.name} className="rounded-md border border-w-line bg-w-field/70 px-2.5 py-1.5 text-w-sm">
              <div className="flex items-center gap-2">
                <b className="min-w-0 flex-1 truncate font-mono">{s.name}</b>
                <span className={cn("text-w-xs", s.granted ? "text-w-teal" : s.in_environment ? "text-w-text2" : "text-w-warn")}>
                  {s.granted ? "Equipped" : s.in_environment ? "In your environment" : "Not in your environment"}
                </span>
              </div>
              {!s.in_environment && !blocked && status.environment_configured && (
                <form
                  className="mt-1.5 flex gap-1.5"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void addAndEquip(s.name);
                  }}
                >
                  <input
                    type="password"
                    autoComplete="new-password"
                    aria-label={`Value for ${s.name}`}
                    placeholder="Paste the value"
                    className={cn(INPUT, "py-1 font-mono text-w-xs")}
                    value={values[s.name] ?? ""}
                    disabled={busy}
                    onChange={(e) => setValues((v) => ({ ...v, [s.name]: e.target.value }))}
                  />
                  <button type="submit" disabled={busy || !values[s.name]} className={BTN_PRIMARY_SM}>
                    Add and equip
                  </button>
                </form>
              )}
            </li>
          ))}
        </ul>
        {!status.environment_configured && <p className="text-w-xs text-w-muted">Secure environment storage is not configured by the operator, so keys cannot be added yet.</p>}
        {secrets.some((s) => !s.in_environment) && status.environment_configured && !blocked && (
          <p className="text-w-xs text-w-muted">Values are stored in You, Environment and are never shown again. You can also add them there.</p>
        )}
        {blocked && <p className="text-w-sm text-w-warn">{blocked}</p>}
        {error && <p role="alert" className="text-w-sm text-w-danger">{error}</p>}
        <div className="flex justify-end gap-2">
          <button type="button" onClick={onClose} className={BTN_SECONDARY}>
            {pendingNames(status, only).length ? "Not now" : "Done"}
          </button>
          <button type="button" disabled={busy || !!blocked || !ready.length} onClick={() => void equip(ready)} className={BTN_PRIMARY}>
            {busy ? "Equipping" : ready.length > 1 ? `Equip ${ready.length}` : "Equip"}
          </button>
        </div>
      </motion.div>
    </motion.div>
  );
}

export function OmpModelsSection({ agent, canChange }: { agent: WorldAgent; canChange: boolean }) {
  const [status, setStatus] = useState<OmpConfig | null>(null);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const onOmp = agent.cli === "omp" || agent.pending_switch?.cli === "omp";

  const load = useCallback(() => {
    worldApi
      .ompConfig(agent.id)
      .then((next) => {
        setStatus(next);
        setText(next.config ?? "");
      })
      .catch(() => setError("Could not load the omp config."));
  }, [agent.id]);

  useEffect(() => {
    if (canChange && onOmp) load();
  }, [canChange, onOmp, load]);

  if (!canChange || !onOmp) return null;

  const saved = status?.config ?? "";
  const dirty = text.trim() !== saved.trim();
  const pending = status ? pendingNames(status) : [];

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await worldApi.saveOmpConfig(agent.id, text);
      setStatus(next);
      setText(next.config ?? "");
      if (pendingNames(next).length) setAsking(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not save that config.");
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      await worldApi.deleteOmpConfig(agent.id);
      load();
    } catch {
      setError("Could not remove the config.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={SECTION}>
      <div className="flex items-start gap-3">
        <Cpu size={20} weight="bold" className="mt-0.5 text-w-teal" />
        <span className="min-w-0 flex-1 text-w-sm text-w-text2">
          {agent.name}&apos;s own omp models
          <span className="block text-w-xs text-w-muted">
            Paste a models.yml. Its providers are added on top of the default ones. Write keys as {"${secret:NAME}"}, never the raw value.
          </span>
        </span>
      </div>
      <textarea
        aria-label="omp models.yml"
        spellCheck={false}
        rows={Math.min(18, Math.max(6, text.split("\n").length + 1))}
        className={cn(INPUT, "w-full resize-y font-mono text-w-xs leading-snug")}
        placeholder={EXAMPLE}
        value={text}
        disabled={busy || !status}
        onChange={(e) => setText(e.target.value)}
      />
      {status?.masked && <p className="text-w-xs text-w-muted">Raw values are hidden as ***. Leave them as they are to keep them.</p>}
      {!!status?.models.length && !dirty && (
        <p className="text-w-xs text-w-text2">
          Models: <span className="font-mono">{status.models.map((m) => m.id).join(", ")}</span>
        </p>
      )}
      {!!pending.length && !dirty && (
        <button type="button" onClick={() => setAsking(true)} className={cn(CHUNKY, "flex items-center gap-1.5 bg-w-lemon px-2.5 py-1 text-w-xs")}>
          <Key size={13} weight="bold" />
          {pending.length === 1 ? `${pending[0].name} is not equipped` : `${pending.length} keys are not equipped`}
        </button>
      )}
      <div className="flex flex-wrap items-center justify-end gap-2">
        {saved && (
          <button type="button" disabled={busy} onClick={() => void remove()} className={BTN_SECONDARY}>
            Remove
          </button>
        )}
        <button type="button" disabled={busy || !dirty || !text.trim()} onClick={() => void save()} className={cn(BTN, BTN_MD, "bg-w-gold hover:bg-w-gold2")}>
          {busy ? "Saving" : "Save"}
        </button>
      </div>
      {error && <p role="alert" className="text-w-sm text-w-danger">{error}</p>}
      <AnimatePresence>
        {asking && status && (
          <EquipSecretsDialog key="equip" agentId={agent.id} agentName={agent.name} status={status} onChanged={setStatus} onClose={() => setAsking(false)} />
        )}
      </AnimatePresence>
    </section>
  );
}

export function EquipCard({ agentId, agentName, keys }: { agentId: string; agentName: string; keys: string[] }) {
  const [status, setStatus] = useState<OmpConfig | null>(null);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    worldApi.ompConfig(agentId).then(setStatus).catch(() => {});
  }, [agentId]);
  if (!status) return null;
  const left = pendingNames(status, keys);
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2">
      {keys.map((k) => (
        <span key={k} className="rounded-md border border-w-line bg-w-field/70 px-1.5 py-0.5 font-mono text-w-xs">
          {k}
        </span>
      ))}
      {left.length ? (
        <button type="button" onClick={() => setOpen(true)} className={BTN_PRIMARY_SM}>
          <Key size={13} weight="bold" />
          Equip
        </button>
      ) : (
        <span className="text-w-xs text-w-teal">Equipped</span>
      )}
      <AnimatePresence>
        {open && <EquipSecretsDialog key="equip" agentId={agentId} agentName={agentName} status={status} only={keys} onChanged={setStatus} onClose={() => setOpen(false)} />}
      </AnimatePresence>
    </div>
  );
}
