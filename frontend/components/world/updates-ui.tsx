"use client";

import { useEffect, useState } from "react";
import { desktopApi, isDesktopApp, UPDATE_EVENT, type UpdateView } from "@/lib/desktop";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM } from "./styles";
import { Shimmer } from "./shimmer";
import { SettingsCard, SettingsGroup, SettingsRow, Switch } from "./settings-shell";

export type UpdatesState = { view: UpdateView | null; canAuto: boolean; setView: (view: UpdateView) => void };

export function useUpdates(): UpdatesState {
  const [view, setView] = useState<UpdateView | null>(null);
  const [canAuto, setCanAuto] = useState(false);
  useEffect(() => {
    if (!isDesktopApp()) return;
    let live = true;
    void desktopApi
      .status()
      .then((status) => {
        if (!status.features?.includes("updates")) return null;
        if (live) setCanAuto(status.features.includes("update_auto"));
        return desktopApi.updateStatus();
      })
      .then((current) => {
        if (live && current) setView(current);
      })
      .catch(() => {});
    const onUpdate = (event: Event) => {
      const next = (event as CustomEvent<UpdateView>).detail;
      if (next) setView(next);
    };
    window.addEventListener(UPDATE_EVENT, onUpdate);
    return () => {
      live = false;
      window.removeEventListener(UPDATE_EVENT, onUpdate);
    };
  }, []);
  return { view, canAuto, setView };
}

const BUSY = new Set(["checking", "downloading", "installing"]);

function Status({ view }: { view: UpdateView }) {
  const next = view.version ? `Loka ${view.version}` : "the new version";
  switch (view.state) {
    case "checking":
      return <Shimmer>Checking for updates</Shimmer>;
    case "downloading":
      return <Shimmer>{`Downloading ${next}${view.progress != null ? ` (${Math.round(view.progress * 100)}%)` : ""}`}</Shimmer>;
    case "installing":
      return <Shimmer>{`Installing ${next} and restarting`}</Shimmer>;
    case "waiting":
      return <Shimmer>{`Restarting for ${next} when your agents finish`}</Shimmer>;
    case "ready":
      return <>{`${next} is ready to install.`}</>;
    case "current":
      return <>You have the latest version.</>;
    case "failed":
      return <>{`Could not check for updates${view.error ? `: ${view.error}` : "."}`}</>;
    default:
      return <>Loka checks for updates when it opens and every 6 hours.</>;
  }
}

export function UpdatesPage({ state, autoNote }: { state: UpdatesState; autoNote?: string }) {
  const { view, canAuto, setView } = state;
  const [error, setError] = useState<string | null>(null);
  if (!view) return null;
  const act = async (run: () => Promise<unknown>) => {
    setError(null);
    try {
      await run();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const setAuto = (enabled: boolean) =>
    act(async () => {
      setView({ ...view, auto_install: enabled });
      try {
        setView(await desktopApi.setAutoUpdate(enabled));
      } catch (e) {
        setView({ ...view, auto_install: !enabled });
        throw e;
      }
    });
  return (
    <SettingsGroup title="Updates">
      <SettingsCard rows>
        <SettingsRow
          title={<span className="font-mono">{`Loka ${view.current}`}</span>}
          description={<Status view={view} />}
          control={
            <>
              {view.state === "ready" && (
                <button type="button" className={BTN_PRIMARY_SM} onClick={() => act(() => desktopApi.installUpdate())}>
                  Restart now
                </button>
              )}
              {view.state === "waiting" && (
                <button type="button" className={BTN_SECONDARY_SM} onClick={() => act(() => desktopApi.stopWaitingForUpdate())}>
                  Not yet
                </button>
              )}
              {view.state !== "ready" && view.state !== "waiting" && (
                <button type="button" className={BTN_SECONDARY_SM} disabled={BUSY.has(view.state)} onClick={() => act(() => desktopApi.checkForUpdates())}>
                  Check for updates
                </button>
              )}
            </>
          }
        />
        {canAuto && (
          <SettingsRow
            title="Update automatically"
            description={`Loka downloads new versions on its own and restarts once your agents finish their work. Turn it off to be asked first.${autoNote ? ` ${autoNote}` : ""}`}
            control={<Switch label="Update automatically" checked={!!view.auto_install} onChange={(on) => void setAuto(on)} />}
          />
        )}
      </SettingsCard>
      {error && <p className="mt-2 text-w-xs text-w-danger">{error}</p>}
    </SettingsGroup>
  );
}
