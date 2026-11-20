"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { AppWindow, ChatsCircle, ComputerTower, Desktop, FolderOpen, Monitor, Pause, Play, Plug, SignOut, TerminalWindow } from "@phosphor-icons/react";
import { desktopApi, isDesktopApp, type DesktopSlack, type DesktopStatus } from "@/lib/desktop";
import { cn } from "@/lib/utils";
import { OPENCODE_OFF_NOTE, WorldApiError, cliOptions, runnerHasOpencode, worldApi, type RunnerView, type WorldAgent } from "@/lib/world";
import { WSelect } from "./controls";
import { BTN_PRIMARY, BTN_SECONDARY, CHUNKY, HUD_CHIP, INPUT } from "./styles";
import { refreshReadiness } from "./setup-hint";
import { VoiceField } from "./voice-ui";
import { pollWhileVisible } from "@/lib/visible-poll";
import { EmptyState } from "./settings-shell";
import { ConnectionMark, deviceConnection, planeConnection } from "./connection-mark";

function age(value: string | null): string {
  if (!value) return "Never connected";
  const seconds = Math.max(0, Math.floor((Date.now() - new Date(value).getTime()) / 1000));
  if (seconds < 60) return "Seen just now";
  if (seconds < 3600) return `Seen ${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `Seen ${Math.floor(seconds / 3600)}h ago`;
  return `Seen ${Math.floor(seconds / 86400)}d ago`;
}

export function DesktopStatusButton({ controlPlane, onOpen }: { controlPlane: "connecting" | "online" | "offline"; onOpen: () => void }) {
  const [native, setNative] = useState<DesktopStatus | null>(null);
  const [fleet, setFleet] = useState<RunnerView[]>([]);
  const [fleetReady, setFleetReady] = useState(false);

  useEffect(() => {
    let stopped = false;
    const load = () => {
      if (isDesktopApp()) {
        void desktopApi.status().then((status) => !stopped && setNative(status), () => !stopped && setNative(null));
      }
      void worldApi.runners().then((runners) => {
        if (stopped) return;
        setFleet(runners);
        setFleetReady(true);
      }, () => {
        if (stopped) return;
        setFleet([]);
        setFleetReady(false);
      });
    };
    load();
    const stop = pollWhileVisible(load, 60_000);
    window.addEventListener("world-reconnected", load);
    return () => {
      stopped = true;
      stop();
      window.removeEventListener("world-reconnected", load);
    };
  }, []);

  const nativeOwned = Boolean(native?.runner_id && fleet.some((runner) => runner.id === native.runner_id));
  const localStatus = native?.runner_id ? (nativeOwned ? native.status : fleetReady ? "error" : null) : native?.status ?? null;
  const online = fleet.filter((runner) => runner.status === "online").length;
  const label = native?.runner_id
    ? nativeOwned
      ? native.name || "This Mac"
      : fleetReady
        ? "Account changed"
        : "Devices"
    : native
      ? "Connect Mac"
      : online
        ? `${online} device${online === 1 ? "" : "s"}`
        : "Devices";
  const plane = planeConnection(controlPlane);
  const device = localStatus === null && !native && fleet.length === 0 ? "disabled" : deviceConnection(localStatus ?? (online ? "online" : "offline"));
  const state = plane === "ok" ? device : plane;
  const stateLabel =
    plane === "connecting" ? "Connecting to Loka" : plane === "off" ? "Loka is not reachable" : device === "ok" ? "All connected" : device === "connecting" ? "Device connecting" : device === "paused" ? "Device paused" : device === "disabled" ? "No device yet" : "Device not connected";
  return (
    <button type="button" onClick={onOpen} title={`${label}: ${stateLabel}`} aria-label={`${label}: ${stateLabel}`} className={cn(HUD_CHIP, "hidden h-11 items-center gap-2 px-3 text-w-sm font-semibold text-w-text lg:flex min-[1680px]:gap-2.5 min-[1680px]:pr-3.5")}>
      <Desktop size={18} weight="bold" className="shrink-0" />
      <span className="hidden max-w-[9rem] truncate min-[1680px]:inline">{label}</span>
      <ConnectionMark state={state} size={16} label={stateLabel} />
    </button>
  );
}

export function DevicesPanel({ controlPlane, embedded = false }: { controlPlane: "connecting" | "online" | "offline"; embedded?: boolean }) {
  const [runners, setRunners] = useState<RunnerView[]>([]);
  const [agents, setAgents] = useState<WorldAgent[]>([]);
  const [native, setNative] = useState<DesktopStatus | null>(null);
  const [desktop, setDesktop] = useState(false);
  const [name, setName] = useState("My Mac");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [loadingRunners, setLoadingRunners] = useState(true);
  const [fleetReady, setFleetReady] = useState(false);
  const [armed, setArmed] = useState(false);

  const refreshNative = useCallback(async () => {
    if (!isDesktopApp()) return;
    setDesktop(true);
    try {
      const status = await desktopApi.status();
      setNative(status);
      if (status.name) setName(status.name);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Could not read this Mac's status.");
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    const loadRunners = () => {
      worldApi.runners(controller.signal).then((list) => {
        setRunners(list);
        setFleetReady(true);
        setUnsupported(false);
      }).catch((reason) => {
        if (controller.signal.aborted) return;
        setUnsupported(reason instanceof Error && reason.message.startsWith("404"));
        setFleetReady(false);
        if (!(reason instanceof Error) || !reason.message.startsWith("404")) setError("Could not load your devices.");
      }).finally(() => !controller.signal.aborted && setLoadingRunners(false));
    };
    loadRunners();
    worldApi.agents().then((list) => { if (!controller.signal.aborted) setAgents(list); }).catch(() => {});
    void refreshNative();
    const stopRunners = pollWhileVisible(loadRunners, 30_000);
    const stopNative = isDesktopApp() ? pollWhileVisible(() => void refreshNative(), 10_000) : null;
    return () => {
      controller.abort();
      stopRunners();
      stopNative?.();
    };
  }, [refreshNative]);

  const run = async (key: string, action: () => Promise<DesktopStatus>) => {
    setBusy(key);
    setError(null);
    try {
      const status = await action();
      setNative(status);
      const next = await worldApi.runners().catch(() => null);
      if (next) {
        setRunners(next);
        setFleetReady(true);
      }
      refreshReadiness();
    } catch (reason) {
      const message = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : "";
      setError(message && message !== "desktop_unavailable" ? message : "That action did not work.");
    } finally {
      setBusy(null);
    }
  };

  const nativeOwned = Boolean(native?.runner_id && runners.some((runner) => runner.id === native.runner_id));
  const nativeMismatch = Boolean(native?.runner_id && fleetReady && !nativeOwned);

  return (
    <section id="world-devices" tabIndex={-1} className={cn("scroll-mt-5 outline-none", !embedded && "rounded-xl border-2 border-w-line bg-w-field/50 p-4 focus-visible:ring-2 focus-visible:ring-w-teal")}>
      <div className="flex items-center justify-between gap-3">
        {embedded ? <span className="text-w-sm text-w-text2">This Mac</span> : (
          <div>
            <h3 className="text-w-sm font-semibold text-w-strong">Execution devices</h3>
            <p className="mt-1 text-w-xs text-w-muted">Agents use these computers for repositories and coding tools.</p>
          </div>
        )}
        <span className="inline-flex items-center gap-1.5 text-w-xs text-w-text2">
          <ConnectionMark state={planeConnection(controlPlane)} />
          Loka {controlPlane}
        </span>
      </div>

      {unsupported ? (
        <p role="alert" className="mt-3 rounded-lg border-2 border-[#e0a08a] bg-w-rose p-3 text-w-sm text-w-danger">The server needs the desktop runner update before devices can connect.</p>
      ) : (
        <ul className="mt-3 space-y-2">
          {loadingRunners && <li className="rounded-lg border-2 border-dashed border-w-line2 p-3 text-w-sm text-w-muted">Loading devices...</li>}
          {!loadingRunners && runners.length === 0 && (
            <li>
              <EmptyState icon={Desktop} title="No devices connected" description="Connect this Mac below so your agents can work here." />
            </li>
          )}
          {runners.map((runner) => (
            <li key={runner.id} className="flex flex-wrap items-center gap-3 rounded-lg border-2 border-w-line bg-w-card p-3">
              <ComputerTower size={20} weight="bold" className="text-w-link" />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-w-base font-semibold">{runner.name}</span>
                <span className="block text-w-xs text-w-muted">{runner.platform} · {age(runner.last_seen_at)}</span>
                <span className="block truncate text-w-xs text-w-text2">Agents: {agents.filter((agent) => agent.runner_id === runner.id).map((agent) => agent.name).join(", ") || "none"}</span>
              </span>
              <span className="inline-flex items-center gap-1.5 text-w-xs capitalize"><ConnectionMark state={deviceConnection(runner.status)} />{runner.status}</span>
              {runner.platform === "macos" && (runner.capabilities?.screen === true || runner.capabilities?.apps === true) && (
                <DeviceAccess
                  runner={runner}
                  onChanged={(updated) => {
                    setRunners((current) => current.map((item) => (item.id === updated.id ? { ...item, screen_access: updated.screen_access, app_control: updated.app_control } : item)));
                    refreshReadiness();
                  }}
                />
              )}
            </li>
          ))}
        </ul>
      )}

      {desktop && !unsupported && nativeMismatch ? (
        <p role="alert" className="mt-4 border-t-2 border-dashed border-w-line2 pt-3 text-w-sm text-w-danger">This Mac is connected to a different Loka database. Disconnect it from the menu bar app and connect it again.</p>
      ) : desktop && !unsupported && (!native?.runner_id || nativeOwned) ? (
        <div className="mt-4 border-t-2 border-dashed border-w-line2 pt-4">
          <p className="text-w-sm font-semibold text-w-strong">This Mac</p>
          {native?.status === "unpaired" || !native?.runner_id ? (
            <div className="mt-2 flex gap-2">
              <input className={cn(INPUT, "min-w-0 flex-1")} value={name} maxLength={60} onChange={(event) => setName(event.target.value)} aria-label="Device name" />
              <button type="button" disabled={busy !== null || !name.trim()} onClick={() => void run("pair", () => desktopApi.pair(name.trim()))} className={BTN_PRIMARY}><Plug size={15} weight="bold" />Connect this Mac</button>
            </div>
          ) : (
            <div className="mt-2 space-y-3">
              <div className="flex flex-wrap gap-2">
                {native.status === "paused" ? (
                  <button type="button" disabled={busy !== null} onClick={() => void run("resume", desktopApi.resume)} className={BTN_PRIMARY}><Play size={14} weight="fill" />Resume</button>
                ) : (
                  <button type="button" disabled={busy !== null} onClick={() => void run("pause", desktopApi.pause)} className={BTN_SECONDARY}><Pause size={14} weight="fill" />Pause</button>
                )}
                <button type="button" disabled={busy !== null} onClick={() => void run("repository", desktopApi.chooseRepository)} className={BTN_SECONDARY}><FolderOpen size={14} weight="bold" />Choose repository</button>
                <button type="button" disabled={busy !== null} onClick={() => void desktopApi.openLogs().catch(() => setError("Could not open logs."))} className={BTN_SECONDARY}><TerminalWindow size={14} weight="bold" />Open logs</button>
                <button type="button" disabled={busy !== null} onBlur={() => setArmed(false)} onClick={() => armed ? void run("disconnect", desktopApi.disconnect) : setArmed(true)} className={cn(CHUNKY, "flex items-center gap-1.5 px-3 py-1.5 text-w-sm", armed ? "bg-w-alert text-white" : "bg-w-card")}><SignOut size={14} weight="bold" />{armed ? "Disconnect this Mac?" : "Disconnect"}</button>
              </div>
              <div className="grid gap-2 text-w-xs text-w-text2 sm:grid-cols-2">
                <p><span className="text-w-muted">Workspace</span><br /><span className="select-text font-mono">{native.workspace_root || "Choose a repository"}</span></p>
                <p><span className="text-w-muted">Coding tools</span><br />{native.backends.length ? native.backends.map((backend) => `${backend.name}: ${backend.installed ? backend.authenticated === false ? "sign in needed" : "ready" : "not installed"}`).join(" · ") : "None detected"}</p>
              </div>
              {native.start_at_login !== undefined && <label className="flex items-center gap-2 text-w-sm"><input type="checkbox" checked={native.start_at_login} onChange={(event) => void run("start", () => desktopApi.setStartAtLogin(event.target.checked))} className="h-4 w-4 accent-w-teal" />Start Loka when I sign in to this Mac</label>}
              {native.slack && <SlackCapture slack={native.slack} busy={busy !== null} onToggle={(enabled) => void run("slack", () => desktopApi.setSlackCapture(enabled))} />}
            </div>
          )}
          {native?.last_error && <p role="alert" className="mt-2 text-w-xs text-w-danger">{native.last_error}</p>}
        </div>
      ) : !desktop ? (
        <p className="mt-4 border-t-2 border-dashed border-w-line2 pt-3 text-w-xs text-w-text2">Open Loka in the desktop app to manage this Mac, choose repositories and check local coding tools.</p>
      ) : null}
      {error && <p role="alert" className="mt-3 text-w-xs text-w-danger">{error}</p>}
    </section>
  );
}

const ACCESS = [
  {
    key: "screen_access",
    capability: "screen",
    icon: Monitor,
    title: "Let quests see this screen",
    body: "Quests on this Mac can take a screenshot or a short recording to show you what they did. macOS also asks you to allow Loka under Privacy & Security, Screen Recording.",
  },
  {
    key: "app_control",
    capability: "apps",
    icon: AppWindow,
    title: "Let quests use apps on this Mac",
    body: "Quests can open and click through apps like Finder, Xcode or Simulator. Each quest asks you first, and nothing runs until you allow it for that quest. macOS also asks you to allow Loka under Privacy & Security, Accessibility.",
  },
] as const;

function DeviceAccess({ runner, onChanged }: { runner: RunnerView; onChanged: (runner: RunnerView) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const toggle = async (key: (typeof ACCESS)[number]["key"], enabled: boolean) => {
    setBusy(true);
    setError(null);
    try {
      onChanged(await worldApi.setDeviceAccess(runner.id, { [key]: enabled }));
    } catch (reason) {
      setError(reason instanceof WorldApiError ? reason.message : "Could not change this setting.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="basis-full space-y-2 border-t border-dashed border-w-line2 pt-2">
      {ACCESS.filter((item) => runner.capabilities?.[item.capability] === true).map((item) => (
        <div key={item.key}>
          <label className="flex items-center gap-2 text-w-sm">
            <input type="checkbox" checked={Boolean(runner[item.key])} disabled={busy} onChange={(event) => void toggle(item.key, event.target.checked)} className="h-4 w-4 accent-w-teal" />
            <item.icon size={15} weight="bold" className="text-w-link" />
            <span className="font-semibold">{item.title}</span>
          </label>
          <p className="mt-1 text-w-xs text-w-muted">{item.body} Off by default.</p>
        </div>
      ))}
      {error && <p role="alert" className="mt-1 text-w-xs text-w-danger">{error}</p>}
    </div>
  );
}

function SlackCapture({ slack, busy, onToggle }: { slack: DesktopSlack; busy: boolean; onToggle: (enabled: boolean) => void }) {
  const watching = slack.running && slack.workspaces.length > 0 && !slack.last_error;
  const state = !slack.enabled
    ? "Off"
    : watching
      ? `Watching ${slack.workspaces.join(", ")}`
      : slack.last_error
        ? "Not working yet"
        : slack.running
          ? "Connecting to Slack..."
          : "Not running";
  return (
    <div className="rounded-lg border-2 border-w-line bg-w-card p-3">
      <label className="flex items-center gap-2 text-w-sm">
        <input type="checkbox" checked={slack.enabled} disabled={busy} onChange={(event) => onToggle(event.target.checked)} className="h-4 w-4 accent-w-teal" />
        <ChatsCircle size={15} weight="bold" className="text-w-link" />
        <span className="font-semibold">Capture Slack notifications</span>
        <span className="ml-auto inline-flex items-center gap-1.5 text-w-xs text-w-text2">
          <ConnectionMark state={!slack.enabled ? "disabled" : watching ? "ok" : slack.last_error || !slack.running ? "off" : "connecting"} />
          {state}
        </span>
      </label>
      <p className="mt-1.5 text-w-xs text-w-muted">
        Reads the Slack desktop app signed in on this Mac and adds your DMs, mentions, keyword highlights and thread replies to the bell. Your Slack sign-in never leaves this Mac.
        {slack.enabled && slack.delivered > 0 ? ` ${slack.delivered} captured since Loka started.` : ""}
      </p>
      {slack.enabled && slack.last_error && <p role="alert" className="mt-1.5 text-w-xs text-w-danger">{slack.last_error}</p>}
    </div>
  );
}

export function AgentRunnerSelector({ agentId, cli: initialCli, model: initialModel, onUpdated }: { agentId: string; cli: string; model: string; onUpdated: (agent: WorldAgent) => void }) {
  const [runners, setRunners] = useState<RunnerView[]>([]);
  const [current, setCurrent] = useState<RunnerView | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [handoffRunner, setHandoffRunner] = useState<RunnerView | null>(null);
  const [cli, setCli] = useState(["codex", "cursor", "opencode", "omp"].includes(initialCli) ? initialCli : "claude");
  const [model, setModel] = useState(initialModel);
  const [summary, setSummary] = useState("");
  const busyRef = useRef(true);

  useEffect(() => {
    let stopped = false;
    busyRef.current = true;
    setBusy(true);
    Promise.all([worldApi.runners(), worldApi.agentRunner(agentId)]).then(([list, assignment]) => {
      if (stopped) return;
      setRunners(list);
      setCurrent(assignment.runner);
    }).catch((reason) => {
      if (stopped) return;
      setUnsupported(reason instanceof Error && reason.message.startsWith("404"));
      if (!(reason instanceof Error) || !reason.message.startsWith("404")) setError("Could not load execution devices.");
    }).finally(() => {
      if (stopped) return;
      busyRef.current = false;
      setBusy(false);
    });
    return () => { stopped = true; };
  }, [agentId]);

  const assign = async (runnerId: string) => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await worldApi.setAgentRunner(agentId, runnerId || null);
      setCurrent(result.runner);
      setHandoffRunner(null);
    } catch (reason) {
      if (reason instanceof WorldApiError && reason.status === 409) {
        const target = runners.find((runner) => runner.id === runnerId);
        setHandoffRunner(target ?? null);
        setError(target ? null : reason.message);
      }
      else setError("Could not change the execution device.");
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  const handoff = async (event: React.FormEvent) => {
    event.preventDefault();
    if (busyRef.current || !handoffRunner || !model.trim() || !summary.trim()) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await worldApi.handoffAgent(agentId, handoffRunner.id, cli, model.trim(), summary.trim());
      setCurrent(result.runner);
      setHandoffRunner(null);
      setSummary("");
      try {
        const updated = (await worldApi.agents()).find((agent) => agent.id === agentId);
        if (updated) onUpdated(updated);
        else setError("Handoff succeeded, but agent details could not refresh.");
      } catch {
        setError("Handoff succeeded, but agent details could not refresh.");
      }
    } catch (reason) {
      setError(reason instanceof WorldApiError ? reason.message : "Could not hand off this agent.");
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  if (unsupported) return <p className="text-w-xs text-w-muted">Execution devices will appear after the server update.</p>;
  if (busy && runners.length === 0 && !current) return <p className="text-w-xs text-w-muted">Loading execution devices...</p>;
  const choices = runners.filter((runner) => runner.status === "online" || runner.id === current?.id);
  if (current && !choices.some((runner) => runner.id === current.id)) choices.unshift(current);
  const label = (runner: RunnerView) => runner.name;
  return (
    <div className="space-y-2 rounded-lg border-2 border-w-line2 bg-w-card p-3 text-w-sm">
      <div className="flex items-center gap-3">
        <span className="min-w-0 flex-1"><span className="block text-w-sm font-semibold text-w-strong">Execution device</span><span className="text-w-xs text-w-muted">New quests and follow-ups run on this device.</span></span>
        <WSelect disabled={busy} ariaLabel="Execution device" className="w-56" value={current?.id ?? ""} onChange={(value) => void assign(value)} options={[{ value: "", label: "Not assigned" }, ...choices.map((runner) => ({ value: runner.id, label: label(runner), hint: runner.status }))]} />
      </div>
      <p className="text-w-xs text-w-text2">{current ? <>Runs on <span className="font-semibold">{label(current)}</span> ({current.status})</> : "Not on any device yet. It moves to an online device when work arrives."}</p>
      {!busy && choices.length === 0 && <p className="text-w-xs text-w-muted">Connect this Mac from You to assign this agent.</p>}
      {handoffRunner && (
        <form onSubmit={handoff} className="space-y-2 border-t-2 border-dashed border-w-line2 pt-3">
          <p className="text-w-sm font-semibold text-w-strong">Hand off to {handoffRunner.name}</p>
          <p className="text-w-xs text-w-muted">The current sessions stay in history. A fresh main session starts on the new device with the memory you provide.</p>
          <div className="flex gap-2">
            <WSelect disabled={busy} ariaLabel="Handoff CLI" className="w-auto min-w-[9rem] whitespace-nowrap" value={cli} onChange={setCli} options={cliOptions(runnerHasOpencode(handoffRunner), cli)} />
            <input className={cn(INPUT, "min-w-0 flex-1")} aria-label="Handoff model" placeholder="Model" maxLength={200} value={model} onChange={(event) => setModel(event.target.value)} />
          </div>
          {!runnerHasOpencode(handoffRunner) && <p className="text-w-xs text-w-muted">{OPENCODE_OFF_NOTE}</p>}
          <VoiceField multiline ariaLabel="Carry-over memory" placeholder="What should the new session remember?" value={summary} onChange={setSummary} />
          <div className="flex justify-end gap-2">
            <button type="button" disabled={busy} onClick={() => { setHandoffRunner(null); setSummary(""); }} className={BTN_SECONDARY}>Cancel</button>
            <button type="submit" disabled={busy || !model.trim() || !summary.trim()} className={BTN_PRIMARY}>Start handoff</button>
          </div>
        </form>
      )}
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </div>
  );
}
