"use client";

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { bindTerminalKeys, type KeyProfile, type TerminalAction } from "@/lib/terminal-keys";
import type { TerminalQuote } from "@/lib/terminal-context";
import type { ShellPhase, Typed } from "@/lib/terminal-type";
import { worldApi } from "@/lib/world";
import { wb, type TermEvent, type WbTerm } from "./workbench/bridge";
import { loadTerminalFont, readTerminal, reducedMotion, selectionSpot, TERMINAL_FONTS, terminalFontSize, terminalTheme, typeCommand } from "./terminal-surface";

function b64ToBytes(data: string): Uint8Array {
  const raw = atob(data);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i += 1) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

export function closeLabel(reason: string): string {
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

export type ShellEvents = {
  changed: (id: string) => void;
  shortcut: (id: string, action: TerminalAction) => void;
  focused: (id: string) => void;
  selection: (id: string, spot: { x: number; y: number } | null) => void;
  native: (id: string, native: string | null) => void;
};

export type LocalOpener = (cols: number, rows: number, reattach: string | null, onEvent: (event: TermEvent) => void) => Promise<WbTerm>;

export const homeShell: LocalOpener = (cols, rows, reattach, onEvent) => wb.homeTermOpen(cols, rows, reattach, onEvent);

export function workshopShell(sessionKey: string): LocalOpener {
  return (cols, rows, reattach, onEvent) => wb.termOpen(sessionKey, cols, rows, reattach, onEvent);
}

export type ShellState = { phase: ShellPhase; detail: string; device: string | null; title: string };

const holders = new Map<string, Shell>();

export function shellHeld(native: string): boolean {
  return holders.has(native);
}

export class Shell {
  readonly id: string;
  readonly host: HTMLDivElement;
  readonly term: Terminal;
  private readonly fitter: FitAddon;
  private readonly events: ShellEvents;
  private readonly cleanups: (() => void)[] = [];
  private socket: WebSocket | null = null;
  private nativeId: string | null = null;
  private generation = 0;
  private typing = 0;
  private waiting: { command: string; generation: number; done: (result: Typed | null) => void } | null = null;
  private disposed = false;
  private font = TERMINAL_FONTS;
  private scale = 1;
  phase: ShellPhase = "idle";
  detail = "";
  device: string | null = null;
  title = "";
  liveSince = 0;

  constructor(id: string, events: ShellEvents, profile: KeyProfile) {
    this.id = id;
    this.events = events;
    this.host = document.createElement("div");
    this.host.className = "h-full w-full";
    this.term = new Terminal({
      fontFamily: TERMINAL_FONTS,
      fontSize: terminalFontSize(),
      lineHeight: 1.2,
      letterSpacing: 0,
      theme: terminalTheme(),
      cursorBlink: !reducedMotion(),
      cursorStyle: "block",
      cursorInactiveStyle: "outline",
      scrollback: 8000,
      macOptionIsMeta: true,
      allowProposedApi: true,
      drawBoldTextInBrightColors: false,
      minimumContrastRatio: 1,
    });
    this.fitter = new FitAddon();
    this.term.loadAddon(this.fitter);
    bindTerminalKeys(this.term, (action) => this.events.shortcut(this.id, action), profile);
    const parking = document.createElement("div");
    parking.className = "fixed -left-[9999px] top-0 h-[300px] w-[800px]";
    document.body.appendChild(parking);
    parking.appendChild(this.host);
    this.term.open(this.host);
    parking.remove();
    void loadTerminalFont(terminalFontSize()).then((family) => {
      if (this.disposed) return;
      this.font = family;
      this.fit();
    });
    const textarea = this.term.textarea;
    const onFocus = () => this.events.focused(this.id);
    textarea?.addEventListener("focus", onFocus);
    this.cleanups.push(() => textarea?.removeEventListener("focus", onFocus));
    const place = () => {
      const picked = this.term.hasSelection() && this.term.getSelection().trim();
      this.events.selection(this.id, picked ? selectionSpot(this.term, this.host.querySelector(".xterm-screen")) : null);
    };
    const selecting = this.term.onSelectionChange(place);
    const scrolling = this.term.onScroll(place);
    const input = this.term.onData((data) => this.send(data));
    const resized = this.term.onResize(({ cols, rows }) => this.sendSize(cols, rows));
    const titled = this.term.onTitleChange((title) => {
      this.title = title.replace(/[\u0000-\u001f\u007f]/g, "").trim().slice(0, 80);
      this.events.changed(this.id);
    });
    this.cleanups.push(() => {
      titled.dispose();
      selecting.dispose();
      scrolling.dispose();
      input.dispose();
      resized.dispose();
    });
    const observer = new ResizeObserver(() => this.fit());
    observer.observe(this.host);
    this.cleanups.push(() => observer.disconnect());
  }

  get state(): ShellState {
    return { phase: this.phase, detail: this.detail, device: this.device, title: this.title };
  }

  get busy(): boolean {
    return this.phase === "starting" || this.phase === "connecting" || this.phase === "live";
  }

  get native(): string | null {
    return this.nativeId;
  }

  fit() {
    if (this.disposed || !this.host.isConnected || this.host.clientWidth === 0 || this.host.clientHeight === 0) return;
    const size = Math.max(2, Math.round(terminalFontSize() * this.scale * 100) / 100);
    if (this.term.options.fontSize !== size) this.term.options.fontSize = size;
    if (this.term.options.fontFamily !== this.font) this.term.options.fontFamily = this.font;
    try {
      this.fitter.fit();
    } catch {
      return;
    }
  }

  focus() {
    this.term.focus();
  }

  setScale(scale: number) {
    if (scale === this.scale) return;
    this.scale = scale;
    this.fit();
  }

  read(selectionOnly: boolean, place: string): TerminalQuote | null {
    return readTerminal(this.term, selectionOnly, place);
  }

  private set(phase: ShellPhase, detail?: string) {
    this.phase = phase;
    if (detail !== undefined) this.detail = detail;
    if (phase === "live" && !this.liveSince) {
      this.liveSince = Date.now();
      this.sendSize(this.term.cols, this.term.rows);
    }
    this.events.changed(this.id);
    if (phase === "live") this.flushWaiting();
    else if (phase !== "starting" && phase !== "connecting") this.dropWaiting();
  }

  private send(data: string) {
    if (this.nativeId) {
      void wb.termWrite(this.nativeId, data).catch(() => {});
      return;
    }
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify({ type: "in", data }));
  }

  private sendSize(cols: number, rows: number) {
    if (this.nativeId) {
      void wb.termResize(this.nativeId, cols, rows).catch(() => {});
      return;
    }
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify({ type: "resize", cols, rows }));
  }

  private detach() {
    if (this.nativeId && holders.get(this.nativeId) === this) holders.delete(this.nativeId);
    this.generation += 1;
    this.socket?.close();
    this.socket = null;
    this.nativeId = null;
  }

  async openLocal(open: LocalOpener, reattach: string | null, device: string | null) {
    this.detach();
    const mine = this.generation;
    this.device = device;
    this.title = "";
    this.liveSince = 0;
    this.term.reset();
    this.set("starting", "");
    try {
      const info = await open(this.term.cols, this.term.rows, reattach, (event) => {
        if (mine !== this.generation) return;
        if (event.type === "data") {
          this.term.write(event.data);
          return;
        }
        this.nativeId = null;
        this.events.native(this.id, null);
        this.term.writeln(`\r\n\x1b[2m[${closeLabel("shell_exited")}]\x1b[0m`);
        this.set("ended", "shell_exited");
      });
      if (mine !== this.generation) {
        if (!reattach || reattach !== info.id) void wb.termClose(info.id).catch(() => {});
        return;
      }
      this.nativeId = info.id;
      const before = holders.get(info.id);
      holders.set(info.id, this);
      if (before && before !== this) before.handOver();
      this.events.native(this.id, info.id);
      if (info.exited) {
        this.nativeId = null;
        this.events.native(this.id, null);
        this.set("ended", "shell_exited");
      } else this.set("live");
    } catch (error) {
      if (mine === this.generation) this.set("error", error instanceof Error ? error.message : "could not open a shell");
    }
  }

  async openRemote(device: string) {
    this.detach();
    const mine = this.generation;
    this.device = device;
    this.title = "";
    this.liveSince = 0;
    this.term.reset();
    this.set("starting", "");
    try {
      const opened = await worldApi.openTerminal(device, this.term.cols, this.term.rows);
      if (mine !== this.generation || this.disposed) return;
      const ws = new WebSocket(opened.socket);
      this.socket = ws;
      ws.onopen = () => {
        if (mine !== this.generation) return;
        this.sendSize(this.term.cols, this.term.rows);
        this.set("connecting");
      };
      ws.onmessage = (event) => {
        if (mine !== this.generation) return;
        let frame: { type?: string; [key: string]: string | undefined };
        try {
          frame = JSON.parse(String(event.data));
        } catch {
          return;
        }
        switch (frame.type) {
          case "status":
            this.set(frame.state === "live" ? "live" : "connecting");
            break;
          case "out":
            this.term.write(b64ToBytes(frame.data ?? ""));
            break;
          case "error":
            this.term.writeln(`\x1b[31m${frame.message}\x1b[0m`);
            break;
          case "bye":
            this.term.writeln(`\r\n\x1b[2m[${frame.reason}]\x1b[0m`);
            this.set("ended", frame.reason ?? "");
            break;
          case "closed":
            this.term.writeln(`\r\n\x1b[2m[${closeLabel(frame.reason ?? "")}]\x1b[0m`);
            this.set("ended", frame.reason ?? "");
            break;
        }
      };
      ws.onclose = () => {
        if (mine === this.generation && (this.phase === "live" || this.phase === "connecting")) this.set("ended");
      };
      ws.onerror = () => {
        if (mine === this.generation && (this.phase === "starting" || this.phase === "connecting")) this.set("error", this.detail || "could not reach the API socket");
      };
    } catch (error) {
      if (mine === this.generation) this.set("error", error instanceof Error ? error.message : "could not open a session");
    }
  }

  private handOver() {
    this.generation += 1;
    this.nativeId = null;
    this.set("elsewhere", "");
  }

  kill() {
    if (this.nativeId) {
      void wb.termClose(this.nativeId).catch(() => {});
      this.events.native(this.id, null);
    }
    try {
      this.socket?.send(JSON.stringify({ type: "close" }));
    } catch {
      void 0;
    }
    this.detach();
    this.set("idle", "");
  }

  private flushWaiting() {
    const queued = this.waiting;
    if (!queued) return;
    this.waiting = null;
    if (queued.generation !== this.generation) return queued.done(null);
    void this.typeNow(queued.command).then(queued.done);
  }

  private dropWaiting() {
    const queued = this.waiting;
    this.waiting = null;
    queued?.done(null);
  }

  type(command: string): Promise<Typed | null> {
    this.dropWaiting();
    if (this.phase === "live") return this.typeNow(command);
    return new Promise((done) => {
      this.waiting = { command, generation: this.generation, done };
    });
  }

  private typeNow(command: string): Promise<Typed> {
    const mine = this.generation;
    const turn = (this.typing += 1);
    return typeCommand(this.term, (data) => this.send(data), command, {
      liveSince: this.liveSince,
      alive: () => !this.disposed && this.generation === mine && this.typing === turn && this.phase === "live",
    });
  }

  dispose(end: boolean) {
    if (this.disposed) return;
    if (end) this.kill();
    this.dropWaiting();
    this.disposed = true;
    this.detach();
    for (const clean of this.cleanups) clean();
    this.term.dispose();
    this.host.remove();
  }
}
