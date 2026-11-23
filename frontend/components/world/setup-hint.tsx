"use client";

import { useContext, useEffect, useState } from "react";
import { ArrowRight, Check, CheckCircle, Copy, Play, Prohibit, Terminal, Wrench } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import { worldApi, type FeatureKey, type FeatureReadiness, type Readiness, type SetupAction, type SetupFix } from "@/lib/world";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, CHUNKY } from "./styles";
import { OPEN_TERMINAL_EVENT, RunCommandContext } from "./run-command";

const FRESH_MS = 15_000;
export const SETUP_EVENT = "world-setup";

let cached: { at: number; value: Readiness } | null = null;
let inflight: Promise<Readiness> | null = null;
const listeners = new Set<(value: Readiness) => void>();

function load(force = false): Promise<Readiness> {
  if (!force && cached && Date.now() - cached.at < FRESH_MS) return Promise.resolve(cached.value);
  if (inflight) return inflight;
  inflight = worldApi
    .readiness()
    .then((value) => {
      cached = { at: Date.now(), value };
      listeners.forEach((listener) => listener(value));
      return value;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

export function refreshReadiness() {
  void load(true).catch(() => {});
}

export function useReadiness(): Readiness | null {
  const [value, setValue] = useState<Readiness | null>(cached?.value ?? null);
  useEffect(() => {
    listeners.add(setValue);
    load().then(setValue).catch(() => {});
    return () => {
      listeners.delete(setValue);
    };
  }, []);
  return value;
}

export function useFeature(key: FeatureKey): FeatureReadiness | null {
  return useReadiness()?.features.find((f) => f.key === key) ?? null;
}

export function requestSetup(action: SetupAction) {
  window.dispatchEvent(new CustomEvent<SetupAction>(SETUP_EVENT, { detail: action }));
}

const ACTION_LABEL: Record<SetupAction, string> = {
  devices: "Open devices",
  keys: "Open environment",
  github: "Open GitHub",
  storage: "Open storage",
};

export function SetupHint({
  feature,
  tone = "paper",
  compact = false,
  onAction,
  className,
}: {
  feature: FeatureReadiness;
  tone?: "paper" | "dark";
  compact?: boolean;
  onAction?: () => void;
  className?: string;
}) {
  if (feature.state === "ready") return null;
  const dark = tone === "dark";
  const action = feature.state === "needs_setup" ? feature.action : null;
  const Icon = feature.state === "unavailable" ? Prohibit : Wrench;
  return (
    <div
      role="note"
      className={cn(
        "flex items-start gap-3 rounded-lg border-2 text-left",
        compact ? "p-2.5" : "p-3.5",
        dark ? "border-white/25 bg-white/10 text-w-onscrim" : "border-dashed border-w-line2 bg-w-card text-w-text",
        className,
      )}
    >
      <Icon size={compact ? 16 : 18} weight="bold" className={cn("mt-0.5 shrink-0", dark ? "text-w-crown" : "text-w-warn")} />
      <div className="min-w-0 flex-1">
        <p className={cn("font-semibold", compact ? "text-w-sm" : "text-w-base")}>
          {feature.state === "unavailable" ? `${feature.label} is not available here` : `${feature.label} needs setup`}
        </p>
        <p className={cn("mt-0.5 leading-relaxed", compact ? "text-w-xs" : "text-w-sm", dark ? "text-w-onscrim/75" : "text-w-text2")}>{feature.reason}</p>
        {feature.step && <p className={cn("mt-1 leading-relaxed", compact ? "text-w-xs" : "text-w-sm")}>{feature.step}</p>}
      </div>
      {action && (
        <button
          type="button"
          onClick={() => {
            onAction?.();
            requestSetup(action);
          }}
          className={cn(BTN_PRIMARY_SM, "shrink-0")}
        >
          {ACTION_LABEL[action]}
          <ArrowRight size={12} weight="bold" />
        </button>
      )}
    </div>
  );
}

const LOCAL_FEATURES = new Set<string>(["devices", "terminal", "workshop", "browser", "slack", "live_voice", "keys", "opencode"]);

export function SetupChecklist({ onAction }: { onAction?: () => void }) {
  const readiness = useReadiness();
  if (!readiness) return <p className="text-w-sm text-w-muted">Checking what is set up...</p>;
  const local = readiness.features.filter((f) => LOCAL_FEATURES.has(f.key));
  const ready = local.filter((f) => f.state === "ready");
  const pending = local.filter((f) => f.state === "needs_setup");
  return (
    <div className="space-y-2">
      {pending.map((f) => (
        <SetupHint key={f.key} feature={f} compact onAction={onAction} />
      ))}
      {ready.length > 0 && (
        <ul className="flex flex-wrap gap-1.5 pt-1">
          {ready.map((f) => (
            <li key={f.key} title={f.reason} className="inline-flex items-center gap-1 rounded-md border border-w-line2 bg-w-field px-1.5 py-0.5 text-w-xs text-w-text2">
              <CheckCircle size={12} weight="fill" className="text-w-teal" />
              {f.label}
            </li>
          ))}
        </ul>
      )}
      {readiness.clis.length > 0 && <p className="text-w-xs text-w-muted">CLIs on online devices: {readiness.clis.join(", ")}</p>}
    </div>
  );
}

async function flip(fix: Extract<SetupFix, { kind: "switch" }>): Promise<void> {
  if (fix.setting === "github_feed") await worldApi.setGithub(true);
  else if (fix.setting === "slack_capture") await desktopApi.setSlackCapture(true);
  else if (fix.runner_id) await worldApi.setDeviceAccess(fix.runner_id, fix.setting === "app_control" ? { app_control: true } : { screen_access: true });
}

function SetupFixRow({ fix }: { fix: SetupFix }) {
  const runner = useContext(RunCommandContext);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const act = async (work: () => Promise<void>, after: string) => {
    if (busy) return;
    setBusy(true);
    setProblem(null);
    try {
      await work();
      setDone(after);
      refreshReadiness();
    } catch (err) {
      setProblem(err instanceof Error && err.message ? err.message : "That did not work. Try again.");
    } finally {
      setBusy(false);
    }
  };
  if (fix.kind === "switch") {
    const here = fix.setting !== "slack_capture" || isDesktopApp();
    return (
      <span className="mt-2 flex flex-wrap items-center gap-2">
        {here ? (
          <button type="button" disabled={busy || !!done} onClick={() => void act(() => flip(fix), "Turned on")} className={BTN_PRIMARY_SM}>
            {done ? <Check size={12} weight="bold" /> : null}
            {done ?? "Turn on"}
          </button>
        ) : (
          <span className="text-w-xs font-semibold text-w-text2">Open the Loka app on your Mac to turn this on.</span>
        )}
        <span className="min-w-0 text-w-xs text-w-muted">{fix.label}</span>
        {problem && <span role="alert" className="block w-full text-w-xs text-w-danger">{problem}</span>}
      </span>
    );
  }
  const canRun = !!runner && !fix.interactive;
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(fix.command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    } catch {
      setCopied(false);
    }
  };
  return (
    <span className="mt-2 block">
      <span className="flex items-center gap-2 rounded-md border border-w-line2 bg-w-sand/60 px-2 py-1.5">
        <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap font-mono text-w-xs text-w-strong">{fix.command}</code>
        <button type="button" onClick={() => void copy()} aria-label={copied ? "Copied" : "Copy command"} title={copied ? "Copied" : "Copy command"} className="grid h-6 w-6 shrink-0 place-items-center rounded-md text-w-muted hover:text-w-strong">
          {copied ? <Check size={13} weight="bold" className="text-w-ok" /> : <Copy size={13} weight="bold" />}
        </button>
      </span>
      <span className="mt-2 flex flex-wrap items-center gap-2">
        {canRun ? (
          <button type="button" disabled={busy || !!done} onClick={() => void act(() => runner.run(fix.command, fix.runner_id), "Started")} className={BTN_PRIMARY_SM}>
            {done ? <Check size={12} weight="bold" /> : <Play size={11} weight="fill" />}
            {done ?? `Run on ${fix.runner}`}
          </button>
        ) : (
          <button type="button" onClick={() => window.dispatchEvent(new CustomEvent(OPEN_TERMINAL_EVENT))} className={BTN_PRIMARY_SM}>
            <Terminal size={12} weight="bold" />
            Open terminal
          </button>
        )}
        <span className="min-w-0 text-w-xs text-w-muted">{canRun ? "The output shows in this chat." : fix.interactive ? `It asks you to sign in, so run it in a terminal on ${fix.runner}.` : `Run it in a terminal on ${fix.runner}.`}</span>
        {problem && <span role="alert" className="block w-full text-w-xs text-w-danger">{problem}</span>}
      </span>
    </span>
  );
}

export function SetupCard({ featureKey }: { featureKey: string }) {
  const readiness = useReadiness();
  const feature = readiness?.features.find((f) => f.key === featureKey) ?? null;
  if (!readiness) return <span className="my-1 block text-w-xs text-w-muted">Checking setup...</span>;
  if (!feature) return <span className="font-mono text-w-xs text-w-muted">setup:{featureKey}</span>;
  if (feature.state === "ready") {
    return (
      <span className="my-1.5 flex items-start gap-2 rounded-lg border-2 border-w-line2 bg-w-card px-3 py-2 text-w-sm">
        <CheckCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-w-ok" />
        <span className="min-w-0">
          <span className="font-semibold">{feature.label} is ready.</span> <span className="text-w-text2">{feature.reason}</span>
        </span>
      </span>
    );
  }
  const off = feature.state === "unavailable";
  const Icon = off ? Prohibit : Wrench;
  return (
    <span role="group" aria-label={`Set up ${feature.label}`} className="my-1.5 block rounded-lg border-2 border-w-line2 bg-w-card p-3 text-left">
      <span className="flex items-start gap-2">
        <Icon size={16} weight="bold" className={cn("mt-0.5 shrink-0", off ? "text-w-muted" : "text-w-warn")} />
        <span className="min-w-0 flex-1">
          <span className="block text-w-sm font-semibold text-w-strong">{off ? `${feature.label} is not available here` : `Set up ${feature.label}`}</span>
          <span className="mt-0.5 block text-w-xs leading-relaxed text-w-text2">{feature.reason}</span>
          {feature.step && <span className="mt-1 block text-w-xs leading-relaxed">{feature.step}</span>}
        </span>
      </span>
      {!off && feature.fix && <SetupFixRow fix={feature.fix} />}
      {!off && (
        <span className="mt-2 flex flex-wrap items-center gap-2">
          {feature.action && (
            <button type="button" onClick={() => requestSetup(feature.action as SetupAction)} className={feature.fix ? BTN_SECONDARY_SM : BTN_PRIMARY_SM}>
              {ACTION_LABEL[feature.action]}
              <ArrowRight size={12} weight="bold" />
            </button>
          )}
          <button type="button" onClick={refreshReadiness} className="text-w-xs font-semibold text-w-muted hover:text-w-strong">
            Check again
          </button>
        </span>
      )}
    </span>
  );
}
