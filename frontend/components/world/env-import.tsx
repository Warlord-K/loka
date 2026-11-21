"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { motion } from "framer-motion";
import { FileArrowUp, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { MAX_IMPORT_BYTES, importProblem, parseDotenv, type DotenvEntry } from "@/lib/dotenv-parse";
import { MODAL_MOTION, SCRIM_MOTION } from "./motion";
import { BTN_GHOST, BTN_PRIMARY, BTN_SECONDARY, INPUT, MODAL, PILL_NEUTRAL, PILL_WARN, SCRIM } from "./styles";
import { CLOSE_BTN } from "./settings-shell";

type Group = "fresh" | "overwrite" | "invalid";

const GROUPS: { key: Group; title: string; hint: string }[] = [
  { key: "fresh", title: "New keys", hint: "Ticked, these are added to your environment." },
  { key: "overwrite", title: "Already saved", hint: "Tick a key to replace the value you saved before." },
  { key: "invalid", title: "Cannot be saved", hint: "Fix these in your file and paste it again." },
];

const PLACEHOLDER = `# Paste the contents of a .env file
OPENAI_API_KEY=sk-...
export LINEAR_API_KEY="lin_api_..."`;

const dots = (value: string) => "•".repeat(Math.min(Math.max(value.length, 4), 12));

export function EnvImportDialog({
  existing,
  onSave,
  onClose,
}: {
  existing: string[];
  onSave: (items: { name: string; value: string }[]) => Promise<void>;
  onClose: () => void;
}) {
  const [text, setText] = useState("");
  const [picks, setPicks] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const saving = useRef(false);
  const close = useRef(onClose);
  close.current = onClose;
  const dismiss = () => {
    if (!saving.current) onClose();
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!saving.current) close.current();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const parsed = useMemo(() => parseDotenv(text), [text]);
  const saved = useMemo(() => new Set(existing), [existing]);
  const groupOf = (entry: DotenvEntry): Group => (entry.problem ? "invalid" : saved.has(entry.name) ? "overwrite" : "fresh");
  const grouped = useMemo(() => {
    const out: Record<Group, DotenvEntry[]> = { fresh: [], overwrite: [], invalid: [] };
    for (const entry of parsed.entries) out[entry.problem ? "invalid" : saved.has(entry.name) ? "overwrite" : "fresh"].push(entry);
    return out;
  }, [parsed, saved]);

  const isPicked = (entry: DotenvEntry) => {
    const group = groupOf(entry);
    if (group === "invalid") return false;
    return picks[entry.name] ?? group === "fresh";
  };
  const chosen = parsed.entries.filter(isPicked);
  const tooLarge = new TextEncoder().encode(text).length > MAX_IMPORT_BYTES
    ? "This paste is too large. Keep it under 8 MB, or split it into smaller imports."
    : importProblem(chosen.map((entry) => ({ name: entry.name, value: entry.value })));
  const replacing = chosen.filter((entry) => saved.has(entry.name)).length;

  const setGroup = (group: Group, on: boolean) =>
    setPicks((current) => ({ ...current, ...Object.fromEntries(grouped[group].map((entry) => [entry.name, on])) }));

  const save = async () => {
    if (!chosen.length || saving.current || tooLarge) return;
    saving.current = true;
    setBusy(true);
    setError(null);
    try {
      await onSave(chosen.map((entry) => ({ name: entry.name, value: entry.value })));
      setText("");
      setPicks({});
      onClose();
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : "Could not save those keys. Nothing was changed.");
    } finally {
      saving.current = false;
      setBusy(false);
    }
  };

  const count = chosen.length;
  const label = busy ? "Saving" : count ? `Save ${count} ${count === 1 ? "key" : "keys"}` : "Save";

  return (
    <motion.div
      {...SCRIM_MOTION}
      className={cn(SCRIM, "z-50")}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) dismiss();
      }}
    >
      <motion.div
        {...MODAL_MOTION}
        role="dialog"
        aria-modal="true"
        aria-label="Import a .env file"
        className={cn(MODAL, "flex max-h-full max-w-2xl flex-col gap-3 p-4")}
      >
        <div className="flex items-center justify-between gap-2">
          <p className="flex items-center gap-2 text-w-sm font-semibold text-w-strong">
            <FileArrowUp size={18} weight="bold" />
            Import a .env file
          </p>
          <button type="button" onClick={dismiss} disabled={busy} aria-label="Close" className={CLOSE_BTN}>
            <X size={16} weight="bold" />
          </button>
        </div>
        <p className="text-w-xs text-w-text2">
          Paste NAME=value lines. Values are encrypted when saved, never shown again, and never kept in this browser.
        </p>
        <textarea
          aria-label=".env contents"
          autoFocus
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          data-1p-ignore
          data-lpignore="true"
          rows={Math.min(10, Math.max(5, text.split("\n").length + 1))}
          className={cn(INPUT, "w-full shrink-0 resize-y font-mono text-w-xs leading-snug")}
          placeholder={PLACEHOLDER}
          value={text}
          disabled={busy}
          onChange={(e) => {
            setText(e.target.value);
            setError(null);
          }}
        />
        {text.trim() && (
          <div className="min-h-0 flex-1 space-y-3 overflow-y-auto pr-1">
            {!parsed.entries.length && <p className="text-w-sm text-w-muted">No NAME=value lines found yet.</p>}
            {GROUPS.map(({ key, title, hint }) =>
              grouped[key].length ? (
                <section key={key} aria-label={title} className="rounded-lg border-2 border-w-line2 bg-w-card p-2.5">
                  <div className="mb-1.5 flex flex-wrap items-center gap-2">
                    <p className="text-w-sm font-semibold text-w-strong">
                      {title} <span className="font-normal text-w-muted">{grouped[key].length}</span>
                    </p>
                    <p className="min-w-0 flex-1 text-w-xs text-w-muted">{hint}</p>
                    {key !== "invalid" && (
                      <span className="flex gap-1">
                        <button type="button" className={cn(BTN_GHOST, "px-1.5 py-0.5 text-w-xs")} disabled={busy} onClick={() => setGroup(key, true)}>
                          All
                        </button>
                        <button type="button" className={cn(BTN_GHOST, "px-1.5 py-0.5 text-w-xs")} disabled={busy} onClick={() => setGroup(key, false)}>
                          None
                        </button>
                      </span>
                    )}
                  </div>
                  <ul className="space-y-1">
                    {grouped[key].map((entry) => {
                      const id = `env-import-${entry.name}`;
                      return (
                        <li key={entry.name} className="rounded-md border border-w-line bg-w-field/70 px-2 py-1.5 text-w-sm">
                          <label htmlFor={id} className={cn("flex items-center gap-2", key === "invalid" ? "cursor-not-allowed opacity-80" : "cursor-pointer")}>
                            <input
                              id={id}
                              type="checkbox"
                              className="h-4 w-4 shrink-0 accent-w-teal"
                              checked={isPicked(entry)}
                              disabled={busy || key === "invalid"}
                              onChange={(e) => setPicks((current) => ({ ...current, [entry.name]: e.target.checked }))}
                            />
                            <b className="min-w-0 truncate font-mono text-w-strong">{entry.name}</b>
                            <span aria-label="Value hidden" className="min-w-0 flex-1 truncate font-mono tracking-wider text-w-muted">
                              {dots(entry.value)}
                            </span>
                            {entry.duplicate && (
                              <span className={PILL_WARN} title="This name appears more than once. The last value is used.">
                                Repeated
                              </span>
                            )}
                            {key === "overwrite" && isPicked(entry) && <span className={PILL_NEUTRAL}>Replaces</span>}
                            <span className="shrink-0 text-w-2xs text-w-muted">line {entry.line}</span>
                          </label>
                          {entry.problem && <p className="mt-1 pl-6 text-w-xs text-w-danger">{entry.problem}</p>}
                        </li>
                      );
                    })}
                  </ul>
                </section>
              ) : null,
            )}
            {parsed.skipped.length > 0 && (
              <section aria-label="Skipped lines" className="rounded-lg border-2 border-dashed border-w-line2 p-2.5">
                <p className="mb-1 text-w-sm font-semibold text-w-strong">Skipped lines</p>
                <ul className="space-y-0.5 text-w-xs text-w-text2">
                  {parsed.skipped.map((skip) => (
                    <li key={`${skip.line}-${skip.reason}`}>
                      Line {skip.line}: {skip.reason}
                    </li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        )}
        {replacing > 0 && (
          <p className="text-w-xs text-w-warn">
            {replacing === 1 ? "1 saved value will be replaced." : `${replacing} saved values will be replaced.`}
          </p>
        )}
        {tooLarge && (
          <p role="alert" className="text-w-sm text-w-danger">
            {tooLarge}
          </p>
        )}
        {error && (
          <p role="alert" className="text-w-sm text-w-danger">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <button type="button" onClick={dismiss} disabled={busy} className={BTN_SECONDARY}>
            Cancel
          </button>
          <button type="button" disabled={busy || !count || !!tooLarge} onClick={() => void save()} className={BTN_PRIMARY}>
            {label}
          </button>
        </div>
      </motion.div>
    </motion.div>
  );
}
