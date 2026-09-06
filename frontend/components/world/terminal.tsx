"use client";

import { bindTerminalKeys } from "@/lib/terminal-keys";
import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { Monitor, Skull, TerminalWindow } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { ConnectionMark } from "./connection-mark";
import { WindowHeader } from "./settings-shell";
import { Shimmer } from "./work-ui";
import { worldApi, type RunnerView } from "@/lib/world";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import { wb } from "./workbench/bridge";
import { BTN_SECONDARY, FOCUS, PANEL, BTN_SECONDARY_DANGER } from "./styles";
import { SetupHint, useFeature } from "./setup-hint";
import { escapeTaken } from "@/lib/escape";

type Phase = "idle" | "starting" | "connecting" | "live" | "ended" | "error";

const THEME = {
  background: "#120c15",
  foreground: "#e9dcc6",
  cursor: "#e8b64a",
  cursorAccent: "#120c15",
  selectionBackground: "#3d2f4a",
  black: "#1d1422",
  red: "#e07a5f",
  green: "#7be06b",
  yellow: "#e8b64a",
  blue: "#6bc7e8",
  magenta: "#c39bff",
  cyan: "#19b8b8",
  white: "#e9dcc6",
  brightBlack: "#5f4f60",
  brightRed: "#ff8fa8",
  brightGreen: "#9bf08b",
  brightYellow: "#f2c66a",
  brightBlue: "#9fdcff",
  brightMagenta: "#d9bfff",
  brightCyan: "#6be0e0",
  brightWhite: "#fff8ea",
};

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

export function TerminalPanel({ meId, onClose }: { meId: string | null; onClose: () => void }) {
  const [devices, setDevices] = useState<RunnerView[] | null>(null);
  const [device, setDevice] = useState<string | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [detail, setDetail] = useState("");
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const socket = useRef<WebSocket | null>(null);
  const native = useRef<string | null>(null);
  const [localRunner, setLocalRunner] = useState<string | null>(null);
  const readiness = useFeature("terminal");

  useEffect(() => {
    if (!isDesktopApp()) return;
    let gone = false;
    desktopApi
      .status()
      .then((status) => {
        if (!gone && status.features?.includes("home_terminal")) setLocalRunner(status.runner_id);
      })
      .catch(() => {});
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

  useEffect(() => {
    if (!host.current || term.current) return;
    const t = new Terminal({ fontFamily: "var(--font-geist-mono), monospace", fontSize: 13, theme: THEME, cursorBlink: true });
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
    const typing = t.onData((data) => {
      if (native.current) {
        void wb.termWrite(native.current, data).catch(() => {});
        return;
      }
      const ws = socket.current;
      if (ws?.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "in", data }));
      }
    });
    return () => {
      observer.disconnect();
      typing.dispose();
      t.dispose();
      term.current = null;
    };
  }, []);

  useEffect(
    () => () => {
      socket.current?.close();
      socket.current = null;
      native.current = null;
    },
    [],
  );

  const open = useCallback(async (id: string) => {
    const t = term.current;
    if (!t) return;
    socket.current?.close();
    socket.current = null;
    native.current = null;
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

  const busy = phase === "starting" || phase === "connecting" || phase === "live";

  useEffect(() => {
    if (busy) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || escapeTaken(e)) return;
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);
  const blocked = devices !== null && devices.length === 0 && !localRunner && readiness && readiness.state !== "ready" ? readiness : null;
  const deviceName = (id: string | null) => devices?.find((d) => d.id === id)?.name ?? "device";

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
          busy && (
            <button type="button" onClick={kill} className={BTN_SECONDARY_DANGER}>
              <Skull size={14} weight="bold" />
              <span className="hidden sm:inline">Kill</span>
            </button>
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
      <div className="relative min-h-0 flex-1 bg-[#120c15]">
        <div ref={host} className="absolute inset-0 px-2 py-1.5" />
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
