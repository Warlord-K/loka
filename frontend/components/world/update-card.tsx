"use client";

import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowsClockwise, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { desktopApi, isDesktopApp, UPDATE_EVENT, type UpdateParts, type UpdateView } from "@/lib/desktop";
import { POPOVER_MOTION } from "./motion";
import { Switch } from "./settings-shell";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, PANEL } from "./styles";
import { Shimmer } from "./work-ui";

const LATER_KEY = "world.update.later";
const VERSION_KEY = "world.update.version";
const PARTS_LATER_KEY = "world.update.parts.later";
const PARTS_SEEN_KEY = "world.update.parts.seen";
const RELOADED_KEY = "world.update.parts.reloaded";

function stored(storage: "local" | "session", key: string): string | null {
  try {
    return (storage === "local" ? window.localStorage : window.sessionStorage).getItem(key);
  } catch {
    return null;
  }
}

function store(storage: "local" | "session", key: string, value: string) {
  try {
    (storage === "local" ? window.localStorage : window.sessionStorage).setItem(key, value);
  } catch {}
}

function partsWorthShowing(parts: UpdateParts | null | undefined): boolean {
  if (!parts) return false;
  if (parts.state === "available") return stored("session", PARTS_LATER_KEY) !== parts.version;
  if (parts.state === "applied") return stored("local", PARTS_SEEN_KEY) !== parts.version;
  if (parts.state === "failed") return stored("session", PARTS_LATER_KEY) !== `failed ${parts.version}` || reloadPending(parts);
  return true;
}

function partNames(parts: UpdateParts): string {
  const names = parts.names.map((name) => (name === "api" ? "server" : "app"));
  return names.length > 1 ? "server and app" : names[0] ?? "app";
}

function reloadPending(parts: UpdateParts | null | undefined): boolean {
  return !!parts && parts.reload && (parts.state === "applied" || parts.state === "failed") && stored("session", RELOADED_KEY) !== parts.version;
}

function reloadForUpdate(parts: UpdateParts) {
  store("session", RELOADED_KEY, parts.version ?? "");
  window.location.reload();
}

function laterFor(version: string | null): boolean {
  try {
    return !!version && window.sessionStorage.getItem(LATER_KEY) === version;
  } catch {
    return false;
  }
}

function seenVersion(current: string): string | null {
  try {
    const seen = window.localStorage.getItem(VERSION_KEY);
    window.localStorage.setItem(VERSION_KEY, current);
    return seen;
  } catch {
    return null;
  }
}

function working(busy: number): string {
  return busy === 1 ? "1 agent is working" : `${busy} agents are working`;
}

function firstLine(notes: string | null): string | null {
  const line = notes?.split("\n").map((l) => l.replace(/^[-*#\s]+/, "").trim()).find(Boolean);
  return line ? (line.length > 140 ? `${line.slice(0, 137)}...` : line) : null;
}

export function UpdateCard() {
  const [view, setView] = useState<UpdateView | null>(null);
  const [updated, setUpdated] = useState<string | null>(null);
  const [hidden, setHidden] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let live = true;
    void desktopApi
      .status()
      .then((status) => (status.features?.includes("updates") ? desktopApi.updateStatus() : null))
      .then((current) => {
        if (!live || !current) return;
        const seen = seenVersion(current.current);
        if (seen && seen !== current.current) setUpdated(current.current);
        if ((current.state === "ready" && !laterFor(current.version)) || current.state === "waiting" || partsWorthShowing(current.parts)) setView(current);
      })
      .catch(() => {});
    const onUpdate = (event: Event) => {
      const next = (event as CustomEvent<UpdateView>).detail;
      if (!next || (next.state === "ready" && laterFor(next.version) && !partsWorthShowing(next.parts))) return;
      setError(null);
      setHidden(false);
      setUpdated(null);
      setView(next);
    };
    window.addEventListener(UPDATE_EVENT, onUpdate);
    return () => {
      live = false;
      window.removeEventListener(UPDATE_EVENT, onUpdate);
    };
  }, []);

  const parts = view?.parts && partsWorthShowing(view.parts) && !["ready", "waiting", "installing"].includes(view.state) ? view.parts : null;

  useEffect(() => {
    if (parts) return;
    if (view?.state !== "current" && !updated) return;
    const id = window.setTimeout(() => (updated ? setUpdated(null) : setHidden(true)), updated ? 8000 : 4000);
    return () => window.clearTimeout(id);
  }, [view, updated, parts]);

  useEffect(() => {
    const pending = view?.parts;
    if (!pending || !reloadPending(pending)) return;
    const away = () => document.hidden || !document.hasFocus();
    if (away()) {
      reloadForUpdate(pending);
      return;
    }
    const onAway = () => {
      if (away()) reloadForUpdate(pending);
    };
    document.addEventListener("visibilitychange", onAway);
    window.addEventListener("blur", onAway);
    return () => {
      document.removeEventListener("visibilitychange", onAway);
      window.removeEventListener("blur", onAway);
    };
  }, [view?.parts]);

  useEffect(() => {
    if (parts?.state !== "applied" || reloadPending(parts)) return;
    const id = window.setTimeout(() => {
      store("local", PARTS_SEEN_KEY, parts.version ?? "");
      setHidden(true);
    }, 8000);
    return () => window.clearTimeout(id);
  }, [parts]);

  const live = !hidden && (view?.state === "ready" || view?.state === "waiting");
  useEffect(() => {
    if (!live) return;
    const id = window.setInterval(() => {
      void desktopApi
        .updateStatus()
        .then((next) => setView((v) => (v && (next.state === "ready" || next.state === "waiting") ? { ...v, busy: next.busy, state: next.state } : v)))
        .catch(() => {});
    }, 5000);
    return () => window.clearInterval(id);
  }, [live]);

  const later = () => {
    try {
      if (view?.version) window.sessionStorage.setItem(LATER_KEY, view.version);
    } catch {}
    setHidden(true);
  };

  const act = async (run: () => Promise<void>, optimistic?: UpdateView["state"]) => {
    setError(null);
    const before = view?.state;
    if (optimistic) setView((v) => (v ? { ...v, state: optimistic } : v));
    try {
      await run();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      if (before) setView((v) => (v ? { ...v, state: before } : v));
    }
  };

  const closeParts = () => {
    if (parts?.state === "available") store("session", PARTS_LATER_KEY, parts.version ?? "");
    if (parts?.state === "applied") store("local", PARTS_SEEN_KEY, parts.version ?? "");
    if (parts?.state === "failed") store("session", PARTS_LATER_KEY, `failed ${parts.version}`);
    setHidden(true);
  };

  const applyParts = async () => {
    setError(null);
    setView((v) => (v?.parts ? { ...v, parts: { ...v.parts, state: "applying", step: "Downloading the update" } } : v));
    try {
      await desktopApi.applyUpdateParts();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const setAutoParts = async (enabled: boolean) => {
    setError(null);
    setView((v) => (v ? { ...v, auto_parts: enabled } : v));
    try {
      const next = await desktopApi.setAutoUpdateParts(enabled);
      setView((v) => (v ? { ...v, auto_parts: next.auto_parts, parts: next.parts ?? v.parts } : v));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setView((v) => (v ? { ...v, auto_parts: !enabled } : v));
    }
  };

  const restart = () => act(() => desktopApi.installUpdate(), "installing");
  const whenIdle = () => act(() => desktopApi.updateWhenIdle(), "waiting");
  const stopWaiting = () => act(() => desktopApi.stopWaitingForUpdate(), "ready");

  const busy = view?.busy ?? 0;
  const show = (!!view && !hidden && (view.state !== "idle" || !!parts)) || !!updated;
  const version = view?.version ? `Loka ${view.version}` : "Loka";
  const note = firstLine(view?.notes ?? null);

  return (
    <div className="pointer-events-none fixed inset-x-0 top-3 z-[60] flex justify-center px-3">
      <AnimatePresence>
        {show && (
          <motion.div
            key="update"
            role="status"
            aria-live="polite"
            {...POPOVER_MOTION}
            className={cn(PANEL, "pointer-events-auto w-full max-w-[24rem] p-4")}
          >
            <div className="flex items-start gap-3">
              <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border-2 border-w-line bg-w-card text-w-link">
                <ArrowsClockwise size={18} weight="duotone" />
              </span>
              <div className="min-w-0 flex-1">
                {parts && <PartsBody parts={parts} auto={!!view?.auto_parts} error={error} onApply={applyParts} onLater={closeParts} onAuto={setAutoParts} />}
                {!parts && updated && !view && (
                  <>
                    <p className="text-w-sm font-semibold text-w-strong">{`Updated to Loka ${updated}`}</p>
                    <p className="mt-1 text-w-xs text-w-text2">Anything your agents were doing picks back up on its own.</p>
                  </>
                )}
                {!parts && view?.state === "checking" && <Shimmer className="text-w-sm font-semibold">Checking for updates</Shimmer>}
                {!parts && view?.state === "downloading" && (
                  <Shimmer className="text-w-sm font-semibold">
                    {`Downloading ${version}${view.progress != null ? ` (${Math.round(view.progress * 100)}%)` : ""}`}
                  </Shimmer>
                )}
                {!parts && view?.state === "installing" && (
                  <>
                    <Shimmer className="text-w-sm font-semibold">{busy > 0 ? "Pausing your agents and restarting" : `Installing ${version} and restarting`}</Shimmer>
                    {busy > 0 && <p className="mt-1 text-w-xs text-w-text2">They pick up where they left off once Loka is back.</p>}
                  </>
                )}
                {!parts && view?.state === "current" && <p className="text-w-sm font-semibold text-w-strong">{`You are on the latest Loka (${view.current}).`}</p>}
                {!parts && view?.state === "failed" && (
                  <>
                    <p className="text-w-sm font-semibold text-w-strong">Could not check for updates.</p>
                    {view.error && <p className="mt-1 line-clamp-3 text-w-xs text-w-muted">{view.error}</p>}
                  </>
                )}
                {!parts && view?.state === "waiting" && (
                  <>
                    <Shimmer className="text-w-sm font-semibold">{`Restarting for ${version} when your agents finish`}</Shimmer>
                    <p className="mt-1 text-w-xs text-w-text2">{busy > 0 ? `${working(busy)}.` : "Almost there."}</p>
                    {error && <p role="alert" className="mt-1 text-w-xs text-w-danger">{error}</p>}
                    <div className="mt-3 flex flex-wrap gap-2">
                      <button type="button" onClick={() => void restart()} className={BTN_PRIMARY_SM}>
                        Restart now
                      </button>
                      <button type="button" onClick={() => void stopWaiting()} className={BTN_SECONDARY_SM}>
                        Not yet
                      </button>
                    </div>
                  </>
                )}
                {!parts && view?.state === "ready" && (
                  <>
                    <p className="text-w-sm font-semibold text-w-strong">{`${version} is ready`}</p>
                    <p className="mt-1 text-w-xs text-w-text2">
                      {busy > 0
                        ? `${working(busy)}. Restarting pauses them for a few seconds and they pick up where they left off.`
                        : note ?? "Restart to finish updating. It takes a few seconds."}
                    </p>
                    {error && <p role="alert" className="mt-1 text-w-xs text-w-danger">{error}</p>}
                    <div className="mt-3 flex flex-wrap gap-2">
                      <button type="button" onClick={() => void restart()} className={BTN_PRIMARY_SM}>
                        Restart now
                      </button>
                      {busy > 0 ? (
                        <button type="button" onClick={() => void whenIdle()} className={BTN_SECONDARY_SM}>
                          When they finish
                        </button>
                      ) : (
                        <button type="button" onClick={later} className={BTN_SECONDARY_SM}>
                          Later
                        </button>
                      )}
                    </div>
                  </>
                )}
              </div>
              {view?.state !== "installing" && parts?.state !== "applying" && (
                <button
                  type="button"
                  onClick={parts ? closeParts : updated && !view ? () => setUpdated(null) : view?.state === "ready" ? later : () => setHidden(true)}
                  aria-label="Close"
                  className="grid h-7 w-7 shrink-0 place-items-center rounded-md text-w-muted transition-colors hover:bg-w-card hover:text-w-text"
                >
                  <X size={14} weight="bold" />
                </button>
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function PartsBody({
  parts,
  auto,
  error,
  onApply,
  onLater,
  onAuto,
}: {
  parts: UpdateParts;
  auto: boolean;
  error: string | null;
  onApply: () => void;
  onLater: () => void;
  onAuto: (enabled: boolean) => void;
}) {
  const what = partNames(parts);
  const autoRow = (
    <label className="mt-3 flex items-center justify-between gap-3 text-w-xs text-w-text2">
      <span>Update the server and app on their own</span>
      <Switch checked={auto} onChange={onAuto} label="Update the server and app on their own" />
    </label>
  );
  if (parts.state === "applying") {
    return (
      <>
        <Shimmer className="text-w-sm font-semibold">{parts.step ?? `Updating the ${what}`}</Shimmer>
        <p className="mt-1 text-w-xs text-w-text2">Your agents keep working while it updates.</p>
      </>
    );
  }
  if (parts.state === "applied") {
    return (
      <>
        <p className="text-w-sm font-semibold text-w-strong">{`Updated the ${what} in place`}</p>
        <p className="mt-1 text-w-xs text-w-text2">
          {reloadPending(parts) ? "Your agents kept working. Loka reloads this window when you switch away." : "Your agents kept working."}
        </p>
        {reloadPending(parts) && (
          <div className="mt-3 flex flex-wrap gap-2">
            <button type="button" onClick={() => reloadForUpdate(parts)} className={BTN_PRIMARY_SM}>
              Reload now
            </button>
          </div>
        )}
      </>
    );
  }
  if (parts.state === "failed") {
    return (
      <>
        <p className="text-w-sm font-semibold text-w-strong">{`Could not update the ${what} in place`}</p>
        {(parts.error || error) && <p role="alert" className="mt-1 line-clamp-4 text-w-xs text-w-muted">{parts.error ?? error}</p>}
        {reloadPending(parts) && (
          <div className="mt-3 flex flex-wrap gap-2">
            <button type="button" onClick={() => reloadForUpdate(parts)} className={BTN_PRIMARY_SM}>
              Reload now
            </button>
          </div>
        )}
      </>
    );
  }
  const note = firstLine(parts.notes);
  return (
    <>
      <p className="text-w-sm font-semibold text-w-strong">{parts.version ? `Loka ${parts.version} is ready` : "A Loka update is ready"}</p>
      <p className="mt-1 text-w-xs text-w-text2">{`It updates the ${what} without stopping your agents.`}</p>
      {note && <p className="mt-1 text-w-xs text-w-muted">{note}</p>}
      {error && <p role="alert" className="mt-1 text-w-xs text-w-danger">{error}</p>}
      <div className="mt-3 flex flex-wrap gap-2">
        <button type="button" onClick={onApply} className={BTN_PRIMARY_SM}>
          Update now
        </button>
        <button type="button" onClick={onLater} className={BTN_SECONDARY_SM}>
          Later
        </button>
      </div>
      {autoRow}
    </>
  );
}
