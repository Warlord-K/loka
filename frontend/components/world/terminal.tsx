"use client";

import { bindTerminalKeys } from "@/lib/terminal-keys";
import type { TerminalQuote } from "@/lib/terminal-context";
import { useCallback, useEffect, useRef, useState, type MutableRefObject, type ReactNode } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { Monitor, Skull, TerminalWindow, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { ConnectionMark } from "./connection-mark";
import { WindowHeader } from "./settings-shell";
import { Shimmer } from "./work-ui";
import { worldApi, type RunnerView } from "@/lib/world";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import { wb } from "./workbench/bridge";
import { BTN_PRIMARY_SM, BTN_SECONDARY_SM, FOCUS, ICON_BTN, PANEL, BTN_SECONDARY_DANGER } from "./styles";
import { SetupHint, useFeature } from "./setup-hint";
import { escapeTaken } from "@/lib/escape";
import { MAIN_TARGET, QuoteButton, sendQuote } from "./quote";
import { TERMINAL_THEME, readTerminal as readFrom, selectionSpot, typeCommand } from "./terminal-surface";
import { planRequest, planSwitch, typedReport, type ShellPhase, type TypedReport } from "@/lib/terminal-type";

type Phase = ShellPhase;

function b64ToBytes(data: string): Uint8Array {
  const raw = atob(data);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

function closeLabel(reason: string): string {
  switch (reason) {
    case "idle":
      return "closed after a while without input";
    case "browser_closed":
      return "you closed the terminal";
    case "runner_closed":
      return "device disconnected";
    case "shell_exited":
      return "shell exited";
    case "never_connected":
      return "device never picked it up";
    default:
      return reason.replace(/_/g, " ");
  }
}

let homeTermId: string | null = null;

export type TerminalReader = () => TerminalQuote | null;

export type TerminalRequest = { id: number; command: string; runnerId: string | null };

export function TerminalPanel({
  meId,
  onClose,
  sideToggle,
  readerRef,
  front = true,
  request = null,
  notice,
  onTyped,
}: {
  meId: string | null;
  onClose: () => void;
  sideToggle?: ReactNode;
  readerRef?: MutableRefObject<TerminalReader | null>;
  front?: boolean;
  request?: TerminalRequest | null;
  notice?: ReactNode;
  onTyped?: (request: number, report: TypedReport | null) => void;
}) {
  const [devices, setDevices] = useState<RunnerView[] | null>(null);
  const [device, setDevice] = useState<string | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [detail, setDetail] = useState("");
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const socket = useRef<WebSocket | null>(null);
  const native = useRef<string | null>(null);
  const [localRunner, setLocalRunner] = useState<string | null>(null);
  const [localChecked, setLocalChecked] = useState(() => !isDesktopApp());
  const generation = useRef(0);
  const phaseRef = useRef<Phase>("idle");
  phaseRef.current = phase;
  const liveSince = useRef(0);
  const deviceRef = useRef<string | null>(null);
  const typing = useRef(0);
  const waiting = useRef<{ command: string; generation: number; request: number } | null>(null);
  const handled = useRef<number | null>(null);
  const [switchAsk, setSwitchAsk] = useState<{ command: string; target: string; request: number } | null>(null);
  const onTypedRef = useRef(onTyped);
  onTypedRef.current = onTyped;
  const [typedNote, setTypedNote] = useState<string | null>(null);
  const readiness = useFeature("terminal");
  const [quoteSpot, setQuoteSpot] = useState<{ x: number; y: number } | null>(null);
  const placeRef = useRef("device");

  useEffect(() => {
    if (!isDesktopApp()) return;
    let gone = false;
    desktopApi
      .status()
      .then((status) => {
        if (!gone && status.features?.includes("home_terminal")) setLocalRunner(status.runner_id);
      })
      .catch(() => {})
      .finally(() => {
        if (!gone) setLocalChecked(true);
      });
    return () => {
      gone = true;
    };
  }, []);

  useEffect(() => {
    let gone = false;
    worldApi
      .runners()
      .then((list) => {
        if (!gone) setDevices(list.filter((r) => r.status === "online" && (!meId || r.owner_user_id === meId)));
      })
      .catch(() => {
        if (!gone) setDevices([]);
      });
    return () => {
      gone = true;
    };
  }, [meId]);

  function sendInput(data: string) {
    if (native.current) {
      void wb.termWrite(native.current, data).catch(() => {});
      return;
    }
    const ws = socket.current;
    if (ws?.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "in", data }));
    }
  }

  useEffect(() => {
    if (!host.current || term.current) return;
    const t = new Terminal({ fontFamily: "var(--font-geist-mono), monospace", fontSize: 13, theme: TERMINAL_THEME, cursorBlink: true });
    const fit = new FitAddon();
    t.loadAddon(fit);
    bindTerminalKeys(t);
    t.open(host.current);
    term.current = t;
    const resize = () => {
      try {
        fit.fit();
      } catch {
        return;
      }
      if (native.current) {
        void wb.termResize(native.current, t.cols, t.rows).catch(() => {});
        return;
      }
      const ws = socket.current;
      if (ws?.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "resize", cols: t.cols, rows: t.rows }));
      }
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(host.current);
    const placeQuote = () => setQuoteSpot(t.hasSelection() && t.getSelection().trim() ? selectionSpot(t, host.current?.querySelector(".xterm-screen")) : null);
    const selecting = t.onSelectionChange(placeQuote);
    const scrolling = t.onScroll(placeQuote);
    const typing = t.onData(sendInput);
    return () => {
      observer.disconnect();
      selecting.dispose();
      scrolling.dispose();
      typing.dispose();
      t.dispose();
      term.current = null;
    };
  }, []);

  useEffect(() => {
    if (phase === "live") liveSince.current = Date.now();
  }, [phase]);

  useEffect(
    () => () => {
      generation.current += 1;
      typing.current += 1;
      waiting.current = null;
      socket.current?.close();
      socket.current = null;
      native.current = null;
    },
    [],
  );

  const open = useCallback(async (id: string) => {
    const t = term.current;
    if (!t) return;
    generation.current += 1;
    phaseRef.current = "starting";
    deviceRef.current = id;
    socket.current?.close();
    socket.current = null;
    native.current = null;
    setSwitchAsk(null);
    setTypedNote(null);
    setDevice(id);
    setPhase("starting");
    setDetail("");
    t.reset();
    t.focus();
    if (id === localRunner) {
      try {
        const info = await wb.homeTermOpen(t.cols, t.rows, homeTermId, (event) => {
          const tt = term.current;
          if (!tt) return;
          if (event.type === "data") {
            tt.write(event.data);
          } else {
            homeTermId = null;
            native.current = null;
            setPhase("ended");
            setDetail("shell_exited");
            tt.writeln(`\r\n\x1b[2m[${closeLabel("shell_exited")}]\x1b[0m`);
          }
        });
        homeTermId = info.id;
        native.current = info.id;
        setPhase(info.exited ? "ended" : "live");
      } catch (error) {
        setPhase("error");
        setDetail(error instanceof Error ? error.message : "could not open a shell");
      }
      return;
    }
    try {
      const opened = await worldApi.openTerminal(id, t.cols, t.rows);
      const ws = new WebSocket(opened.socket);
      socket.current = ws;
      ws.onopen = () => setPhase("connecting");
      ws.onmessage = (event) => {
        let frame: { type?: string; [key: string]: string | undefined };
        try {
          frame = JSON.parse(String(event.data));
        } catch {
          return;
        }
        const tt = term.current;
        if (!tt) return;
        switch (frame.type) {
          case "status":
            setPhase(frame.state === "live" ? "live" : "connecting");
            break;
          case "hello":
            break;
          case "out":
            tt.write(b64ToBytes(frame.data ?? ""));
            break;
          case "error":
            tt.writeln(`\x1b[31m${frame.message}\x1b[0m`);
            break;
          case "bye":
            setPhase("ended");
            setDetail(frame.reason ?? "");
            tt.writeln(`\r\n\x1b[2m[${frame.reason}]\x1b[0m`);
            break;
          case "closed":
            setPhase("ended");
            setDetail(frame.reason ?? "");
            tt.writeln(`\r\n\x1b[2m[${closeLabel(frame.reason ?? "")}]\x1b[0m`);
            break;
        }
      };
      ws.onclose = () => {
        setPhase((prev) => (prev === "live" || prev === "connecting" ? "ended" : prev));
      };
      ws.onerror = () => {
        setPhase((prev) => (prev === "starting" || prev === "connecting" ? "error" : prev));
        setDetail((prev) => prev || "could not reach the API socket");
      };
    } catch (error) {
      setPhase("error");
      setDetail(error instanceof Error ? error.message : "could not open a session");
    }
  }, [localRunner]);

  const kill = useCallback(() => {
    generation.current += 1;
    waiting.current = null;
    setSwitchAsk(null);
    if (native.current) {
      void wb.termClose(native.current).catch(() => {});
      homeTermId = null;
      native.current = null;
    }
    try {
      socket.current?.send(JSON.stringify({ type: "close" }));
    } catch {
      void 0;
    }
    socket.current?.close();
    socket.current = null;
    setPhase("idle");
    setDetail("");
  }, []);

  useEffect(() => {
    if (localRunner && homeTermId && phase === "idle") void open(localRunner);
  }, [localRunner, open, phase]);

  const readTerminal = useCallback((selectionOnly: boolean): TerminalQuote | null => (term.current ? readFrom(term.current, selectionOnly, placeRef.current) : null), []);

  const names = useRef({ devices, localRunner });
  names.current = { devices, localRunner };

  const typeNow = useCallback((command: string, request: number) => {
    const t = term.current;
    if (!t) return;
    const mine = generation.current;
    const turn = (typing.current += 1);
    const current = () => generation.current === mine && typing.current === turn;
    const id = deviceRef.current;
    const device = names.current.devices?.find((d) => d.id === id)?.name ?? (id && id === names.current.localRunner ? "this Mac" : "this device");
    setTypedNote(null);
    void typeCommand(t, sendInput, command, { liveSince: liveSince.current, alive: () => current() && phaseRef.current === "live" }).then((result) => {
      if (!current()) return;
      const report = typedReport(result, device);
      setTypedNote(report?.note ?? null);
      onTypedRef.current?.(request, report);
    });
  }, []);

  useEffect(() => {
    if (!request || handled.current === request.id) return;
    if (devices === null || !localChecked || !term.current) return;
    handled.current = request.id;
    const phaseNow = phaseRef.current;
    const live = phaseNow === "live" || phaseNow === "connecting" || phaseNow === "starting";
    const current = live ? deviceRef.current : null;
    const target = request.runnerId ?? current ?? localRunner ?? devices[0]?.id ?? null;
    typing.current += 1;
    waiting.current = null;
    setSwitchAsk(null);
    const plan = planRequest({ phase: phaseNow, current, target });
    if (plan === "none" || !target) {
      setTypedNote("No device is online to type it on. Pair or wake one in Devices.");
      onTypedRef.current?.(request.id, null);
      return;
    }
    if (plan === "type") return typeNow(request.command, request.id);
    if (plan === "ask") {
      setSwitchAsk({ command: request.command, target, request: request.id });
      return;
    }
    if (plan === "open") void open(target);
    waiting.current = { command: request.command, generation: generation.current, request: request.id };
  }, [request, devices, localChecked, localRunner, open, typeNow]);

  useEffect(() => {
    if (phase !== "live") return;
    const queued = waiting.current;
    if (!queued) return;
    waiting.current = null;
    if (queued.generation === generation.current) typeNow(queued.command, queued.request);
  }, [phase, typeNow]);

  const switchTo = (ask: { command: string; target: string; request: number }, go: boolean) => {
    setSwitchAsk(null);
    typing.current += 1;
    waiting.current = null;
    const { plan, device: id } = planSwitch({ go, phase: phaseRef.current, current: deviceRef.current, target: ask.target });
    if (plan === "none" || !id) {
      onTypedRef.current?.(ask.request, null);
      return;
    }
    if (plan === "type") return typeNow(ask.command, ask.request);
    if (plan === "open") void open(id);
    waiting.current = { command: ask.command, generation: generation.current, request: ask.request };
  };

  useEffect(() => {
    if (!readerRef) return;
    readerRef.current = () => readTerminal(false);
    return () => {
      readerRef.current = null;
    };
  }, [readerRef, readTerminal]);

  const quoteSelection = () => {
    const quote = readTerminal(true);
    setQuoteSpot(null);
    if (!quote) return;
    term.current?.clearSelection();
    sendQuote(MAIN_TARGET, quote);
  };

  const busy = phase === "starting" || phase === "connecting" || phase === "live";

  useEffect(() => {
    if (busy || !front) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || escapeTaken(e)) return;
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, front, onClose]);
  const blocked = devices !== null && devices.length === 0 && !localRunner && readiness && readiness.state !== "ready" ? readiness : null;
  const deviceName = (id: string | null) => devices?.find((d) => d.id === id)?.name ?? "device";
  placeRef.current = device && device === localRunner ? "this Mac" : deviceName(device);

  return (
    <div role="dialog" aria-modal="true" aria-label="Terminal" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <WindowHeader
        title="Terminal"
        subtitle={
          phase === "live" ? (
            <span className="flex items-center gap-1.5">
              <ConnectionMark state="ok" size={12} />
              Live on {deviceName(device)}
            </span>
          ) : phase === "connecting" || phase === "starting" ? (
            <Shimmer>Connecting to {deviceName(device)}</Shimmer>
          ) : phase === "ended" ? (
            detail ? closeLabel(detail) : "Session ended"
          ) : phase === "error" ? (
            <span className="text-w-danger">{detail}</span>
          ) : devices === null ? (
            <Shimmer>Looking for devices</Shimmer>
          ) : devices.length === 0 ? (
            "No devices online"
          ) : (
            "Open a shell on one of your devices"
          )
        }
        onClose={onClose}
        actions={
          (sideToggle || busy) && (
            <>
              {sideToggle}
              {busy && (
                <button type="button" onClick={kill} className={BTN_SECONDARY_DANGER}>
                  <Skull size={14} weight="bold" />
                  <span className="hidden sm:inline">Kill</span>
                </button>
              )}
            </>
          )
        }
      />
      {devices !== null && devices.length > 0 && (
        <div className="flex shrink-0 items-center gap-1.5 overflow-x-auto border-b-2 border-dashed border-w-line2 px-4 py-2 [scrollbar-width:none] sm:px-5" role="toolbar" aria-label="Devices">
          {devices.map((d) => {
            const on = device === d.id && busy;
            return (
              <button
                key={d.id}
                type="button"
                disabled={busy && device === d.id}
                onClick={() => void open(d.id)}
                aria-pressed={on}
                className={cn(
                  "flex shrink-0 items-center gap-2 rounded-lg px-3 py-1.5 text-w-sm transition-colors disabled:cursor-default",
                  FOCUS,
                  "focus-visible:ring-offset-0",
                  on ? "bg-w-paper font-semibold text-w-strong ring-2 ring-inset ring-w-teal/60" : "text-w-text2 hover:bg-w-sand hover:text-w-text",
                )}
              >
                <Monitor size={15} weight={on ? "fill" : "regular"} className={on ? "text-w-link" : "text-w-muted"} />
                {d.name}
                {d.id === localRunner && <span className="rounded bg-w-sand px-1.5 py-px text-w-3xs font-medium text-w-muted">This Mac</span>}
              </button>
            );
          })}
        </div>
      )}
      {switchAsk ? (
        <div role="group" aria-label="Switch device" className="flex shrink-0 flex-wrap items-center gap-2 border-b-2 border-dashed border-w-line2 bg-w-sand px-4 py-2 text-w-sm sm:px-5">
          <span className="min-w-0 flex-1 text-w-text2">
            Switch to {deviceName(switchAsk.target)}? That ends the shell on {deviceName(device)}.
          </span>
          <button type="button" onClick={() => switchTo(switchAsk, true)} className={BTN_PRIMARY_SM}>
            Switch
          </button>
          <button type="button" onClick={() => switchTo(switchAsk, false)} className={BTN_SECONDARY_SM}>
            Type on {deviceName(device)}
          </button>
          <button
            type="button"
            onClick={() => {
              setSwitchAsk(null);
              onTypedRef.current?.(switchAsk.request, null);
            }}
            className={BTN_SECONDARY_SM}
          >
            Cancel
          </button>
        </div>
      ) : (
        (typedNote || notice) && (
          <div role="status" className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b-2 border-dashed border-w-line2 bg-w-sand px-4 py-2 text-w-sm text-w-text2 sm:px-5">
            {typedNote && <span className="min-w-0 flex-1">{typedNote}</span>}
            {notice}
            <button type="button" onClick={() => setTypedNote(null)} aria-label="Dismiss" title="Dismiss" className={cn(ICON_BTN, !typedNote && "hidden")}>
              <X size={14} weight="bold" />
            </button>
          </div>
        )
      )}
      <div className="relative min-h-0 flex-1 bg-[#120c15]">
        <div ref={host} className="absolute inset-0 px-2 py-1.5" />
        <QuoteButton spot={front ? quoteSpot : null} onQuote={quoteSelection} />
        {blocked ? (
          <div className="absolute inset-0 flex items-center justify-center p-6">
            <SetupHint feature={blocked} tone="dark" className="max-w-lg" />
          </div>
        ) : (
          phase === "idle" && (
            <div className="pointer-events-none absolute inset-0 flex flex-col items-center justify-center gap-3 text-center">
              <span className="grid h-12 w-12 place-items-center rounded-2xl bg-white/5 text-[#8f7d90]">
                <TerminalWindow size={24} weight="duotone" />
              </span>
              <p className="text-w-sm text-[#8f7d90]">Pick a device above to open a shell on it</p>
            </div>
          )
        )}
      </div>
    </div>
  );
}
