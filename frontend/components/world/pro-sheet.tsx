"use client";

import { useCallback, useEffect, useState } from "react";
import { ArrowSquareOut, Crown } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { desktopApi, isDesktopApp, UPGRADE_EVENT, type UpgradeAction, type UpgradeView } from "@/lib/desktop";
import { WindowHeader } from "./settings-shell";
import { Shimmer } from "./shimmer";
import { BTN_PRIMARY, BTN_SECONDARY, MODAL } from "./styles";

export const PRO_WAITLIST_URL = "https://loka.ygpt.dev/#pro";

export const PRO_PITCH = "Work together with your team in organizations, use every device including iPhone, iPad and Apple Watch, and share agents and runners.";

const UPGRADE_NOTE = "Upgrading copies your agents and everything they know to Loka Pro. Nothing here is deleted.";
const UPGRADING = "You have Pro, upgrading your Loka";

export function ProSheet({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  if (isDesktopApp()) return <UpgradeSheet onClose={onClose} />;
  return (
    <div role="dialog" aria-modal="true" aria-label="Loka Pro" className={cn(MODAL, "w-full max-w-md")}>
      <WindowHeader title="Loka Pro" subtitle="Coming soon" icon={Crown} onClose={onClose} />
      <div className="space-y-4 px-4 py-4 sm:px-5">
        <p className="text-w-base leading-relaxed text-w-text">
          Loka Pro, coming soon. {PRO_PITCH}
        </p>
        <a href={PRO_WAITLIST_URL} target="_blank" rel="noopener noreferrer" onClick={onClose} className={cn(BTN_PRIMARY, "w-full justify-center")}>
          Join the waitlist
          <ArrowSquareOut size={15} weight="bold" />
        </a>
      </div>
    </div>
  );
}

function bytes(value: number): string {
  if (value >= 1024 * 1024 * 1024) return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  if (value >= 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  if (value >= 1024) return `${Math.round(value / 1024)} KB`;
  return `${value} B`;
}

function moveError(error: string | null): string {
  const text = (error ?? "Could not move your Loka").trim().replace(/\s*Nothing was changed\.?$/i, "").replace(/[.\s]+$/, "");
  return `${text || "Could not move your Loka"}. Nothing was changed.`;
}

function plural(count: number, one: string): string {
  return `${count} ${one}${count === 1 ? "" : "s"}`;
}

function movedLine(view: UpgradeView): string {
  const org = view.org ?? "Loka Pro";
  if (view.agents == null || view.reports == null || view.keys == null) return `Your things are in ${org}.`;
  return `Your things are in ${org}: ${plural(view.agents, "agent")}, ${plural(view.reports, "report")}, ${plural(view.keys, "key")}.`;
}

type Screen = {
  title: string;
  body: React.ReactNode;
  live?: boolean;
  details?: boolean;
  buttons: { label: string; action: UpgradeAction | "start" | "cancel" | "waitlist" | "dismiss"; primary?: boolean }[];
};

function screenFor(view: UpgradeView): Screen {
  const downloading = view.total
    ? `Downloading Loka Pro ${view.version ?? ""}... ${bytes(view.done ?? 0)} of ${bytes(view.total)}`
    : `Downloading Loka Pro ${view.version ?? ""}...`;
  switch (view.step) {
    case "waiting":
      return {
        title: "Finish in your browser",
        body: "Sign in to Loka Pro in the browser window that just opened. If you don't have Pro yet, you can join the waitlist or enter a code there.",
        buttons: [
          { label: "Open the page again", action: "page", primary: true },
          { label: "Cancel", action: "cancel" },
        ],
      };
    case "checking":
      return { title: UPGRADING, body: "Checking your Pro account...", live: true, buttons: [{ label: "Cancel", action: "cancel" }] };
    case "packing":
      return { title: UPGRADING, body: "Packing your Loka...", live: true, buttons: [] };
    case "moving":
      return { title: UPGRADING, body: `Moving your agents to ${view.org ?? "Loka Pro"}...`, live: true, buttons: [] };
    case "downloading":
      return { title: UPGRADING, body: downloading.replace(/\s+\.\.\./, "..."), live: true, buttons: [] };
    case "verifying":
      return { title: UPGRADING, body: "Checking Loka Pro...", live: true, buttons: [] };
    case "installing":
      return { title: UPGRADING, body: "Installing Loka Pro...", live: true, buttons: [] };
    case "opening":
      return { title: "Opening Loka Pro", body: `${movedLine(view)} Loka Pro opens now and this app closes.`, details: true, buttons: [] };
    case "done":
      return {
        title: "Loka Pro is open",
        body: `Sign in there if it asks, using the account you just upgraded with. This app stays installed with your old data, so you can come back to it.${view.login_note ? " Loka Pro can be set to start at login in its settings." : ""}`,
        details: true,
        buttons: [{ label: "Close", action: "close", primary: true }],
      };
    case "moved":
      return {
        title: "Your Loka is in Loka Pro",
        body: "Download Loka Pro to keep working there.",
        details: true,
        buttons: [
          { label: "Download Loka Pro", action: "download", primary: true },
          { label: "Close", action: "close" },
        ],
      };
    case "download_failed":
      return {
        title: "Couldn't download Loka Pro",
        body: `Your things are already in ${view.org ?? "Loka Pro"}. Check your connection and try again.`,
        buttons: [
          ...(view.can_retry ? [{ label: "Try again", action: "again" as const, primary: true }] : []),
          { label: "Download in browser", action: "download", primary: !view.can_retry },
        ],
      };
    case "check_failed":
      return {
        title: "Couldn't check Loka Pro",
        body: "The download didn't match what Loka Pro published, so it wasn't installed.",
        buttons: [
          ...(view.can_retry ? [{ label: "Try again", action: "again" as const, primary: true }] : []),
          { label: "Download in browser", action: "download", primary: !view.can_retry },
        ],
      };
    case "move_failed":
      return {
        title: "Couldn't move your Loka",
        body: moveError(view.error),
        buttons: [
          { label: "Start again", action: "again", primary: true },
          { label: "Close", action: "close" },
        ],
      };
    case "timed_out":
      return {
        title: "The upgrade timed out",
        body: "Nothing was changed. Start again when you are ready.",
        buttons: [
          { label: "Start again", action: "again", primary: true },
          { label: "Close", action: "close" },
        ],
      };
    case "install_blocked":
      return {
        title: "Couldn't install Loka Pro",
        body: view.blocked === "other" ? "Another app is already called Loka Pro in Applications." : "Quit Loka Pro and try again.",
        buttons: [
          { label: "Try again", action: "again", primary: true },
          { label: "Show in Finder", action: "finder" },
        ],
      };
    case "open_failed":
      return {
        title: "Couldn't open Loka Pro",
        body: "It is installed in Applications.",
        buttons: [
          { label: "Open Loka Pro", action: "open", primary: true },
          { label: "Show in Finder", action: "finder" },
        ],
      };
    default:
      return {
        title: "Loka Pro",
        body: `${PRO_PITCH} ${UPGRADE_NOTE}`,
        buttons: [
          { label: "Upgrade to Pro", action: "start", primary: true },
          { label: "Join the waitlist", action: "waitlist" },
        ],
      };
  }
}

function Details({ view }: { view: UpgradeView }) {
  if (!view.renamed.length && !view.needs_values.length && !view.notes.length) return null;
  return (
    <details className="rounded-lg bg-w-card px-3 py-2 text-w-sm text-w-text2">
      <summary className="cursor-pointer select-none font-semibold text-w-text">Details</summary>
      <ul className="mt-2 list-disc space-y-1 pl-5">
        {view.renamed.map((line) => (
          <li key={`r-${line}`}>{line}</li>
        ))}
        {view.needs_values.length > 0 && <li>Add these keys again in Loka Pro under You &gt; Keys: {view.needs_values.join(", ")}</li>}
        {view.notes.map((line) => (
          <li key={`n-${line}`}>{line}</li>
        ))}
      </ul>
    </details>
  );
}

function UpgradeSheet({ onClose }: { onClose: () => void }) {
  const [view, setView] = useState<UpgradeView | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [pending, setPending] = useState(false);

  useEffect(() => {
    let live = true;
    desktopApi.upgradeStatus().then((next) => live && setView(next)).catch(() => {});
    const onUpgrade = (event: Event) => {
      const detail = (event as CustomEvent<UpgradeView>).detail;
      if (detail?.step) setView(detail);
    };
    window.addEventListener(UPGRADE_EVENT, onUpgrade);
    return () => {
      live = false;
      window.removeEventListener(UPGRADE_EVENT, onUpgrade);
    };
  }, []);

  const act = useCallback(
    async (action: Screen["buttons"][number]["action"]) => {
      setProblem(null);
      if (action === "waitlist") {
        window.open(PRO_WAITLIST_URL, "_blank", "noopener,noreferrer");
        onClose();
        return;
      }
      if (action === "dismiss") {
        onClose();
        return;
      }
      setPending(true);
      try {
        const next =
          action === "start"
            ? await desktopApi.upgradeStart()
            : action === "cancel"
              ? await desktopApi.upgradeCancel()
              : await desktopApi.upgradeRetry(action);
        setView(next);
        if (action === "close" || action === "cancel") onClose();
      } catch (error) {
        setProblem(error instanceof Error ? error.message : String(error));
      } finally {
        setPending(false);
      }
    },
    [onClose],
  );

  const screen = screenFor(view ?? ({ step: "idle" } as UpgradeView));
  return (
    <div role="dialog" aria-modal="true" aria-label={screen.title} className={cn(MODAL, "w-full max-w-md")}>
      <WindowHeader title={screen.title} icon={Crown} onClose={onClose} />
      <div className="space-y-4 px-4 py-4 sm:px-5">
        <p className="text-w-base leading-relaxed text-w-text" aria-live="polite">
          {screen.live ? <Shimmer>{screen.body}</Shimmer> : screen.body}
        </p>
        {screen.details && view && <Details view={view} />}
        {problem && <p className="text-w-sm text-w-danger">{problem}</p>}
        {screen.buttons.length > 0 && (
          <div className="flex flex-col gap-2 sm:flex-row-reverse">
            {screen.buttons.map((button) => (
              <button
                key={button.label}
                type="button"
                disabled={pending}
                onClick={() => void act(button.action)}
                className={cn(button.primary ? BTN_PRIMARY : BTN_SECONDARY, "w-full justify-center sm:flex-1")}
              >
                {button.label}
                {(button.action === "download" || button.action === "waitlist") && <ArrowSquareOut size={15} weight="bold" />}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
