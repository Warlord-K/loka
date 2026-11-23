"use client";

import "@xterm/xterm/css/xterm.css";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type MutableRefObject, type ReactNode } from "react";
import { ArrowClockwise, CaretDown, CornersOut, DotsSixVertical, Monitor, Plus, SidebarSimple, Skull, SquareHalfBottom, SquareSplitHorizontal, SquareSplitVertical, TerminalWindow, X } from "@phosphor-icons/react";
import * as PopoverPrimitive from "@radix-ui/react-popover";
import { cn } from "@/lib/utils";
import type { TerminalQuote } from "@/lib/terminal-context";
import { keyProfile, shortcutLabel, terminalShortcut, type LabelledAction, type TerminalAction } from "@/lib/terminal-keys";
import { planRequest, typedReport, type TypedReport } from "@/lib/terminal-type";
import { defaultDevice } from "@/lib/run-device";
import {
  activeTab,
  closePane,
  adoptShells,
  cycleFocus,
  deviceChoices,
  otherDevices,
  cycleTab,
  focusedPane,
  focusPane,
  focusToward,
  freshLayout,
  layoutKey,
  loadLayout,
  moveTab,
  movePane,
  newPane,
  paneToTab,
  panesIn,
  placePane,
  renameTab,
  reorderTab,
  saveLayout,
  selectTab,
  setRatio,
  updatePane,
  workshopLayoutKey,
  embedLayoutKey,
  zoneAt,
  newPaneProblem,
  DEVICE_TERMINALS,
  MAC_TERMINALS_PER_PLACE,
  MAX_PANES,
  publishTerminals,
  blankPane,
  checkUntilAnswered,
  enqueue,
  nextLocalCheck,
  queuedOpens,
  planCommand,
  planRestored,
  waitingNote,
  targetsReady,
  targetsSlow,
  type CommandAction,
  type LocalCheck,
  type QueuedAction,
  type RestoredPlan,
  type Dock,
  type LayoutNode,
  type SplitDir,
  type TermLayout,
  type TermPane,
  type TermTab,
  type TerminalDevice,
  type Zone,
} from "@/lib/terminal-layout";
import { escapeTaken, takeEscape } from "@/lib/escape";
import { worldApi, type RunnerView } from "@/lib/world";
import { desktopApi, isDesktopApp } from "@/lib/desktop";
import { ConnectionMark } from "./connection-mark";
import { MENU, MENU_ITEM } from "./controls";
import { WindowHeader } from "./settings-shell";
import { Shimmer } from "./work-ui";
import { BTN_SECONDARY_DANGER, FOCUS, PANEL } from "./styles";
import { SetupHint, useFeature } from "./setup-hint";
import { MAIN_TARGET, QuoteButton, sendQuote, type MessageQuote } from "./quote";
import { closeLabel, homeShell, Shell, workshopShell, type ShellState } from "./terminal-shell";
import { wb } from "./workbench/bridge";
import { EMBED_DRAG, embedDragData, orphanKeys, terminalTarget } from "@/lib/canvas-embeds";

export type TerminalReader = () => TerminalQuote | null;

export type TerminalRequest = { id: number; command: string; runnerId: string | null };

export type TerminalPlace = { dock: Dock; size: number };

export type TerminalCommand = { id: number; action: CommandAction; tab?: string; device?: string };

export type TerminalSource = { kind: "devices" } | { kind: "workshop"; sessionKey: string } | { kind: "embed"; key: string; device: string; shell: string | null; startsShell: boolean };

const WORKSHOP_PLACE = "the Workshop terminal";

type QueuedCommand = TerminalCommand & QueuedAction;

const DOCK_CHOICES: { dock: Dock; label: string; icon: typeof CornersOut; flip?: boolean }[] = [
  { dock: "window", label: "Open as a window", icon: CornersOut },
  { dock: "bottom", label: "Dock to the bottom", icon: SquareHalfBottom },
  { dock: "left", label: "Dock to the left", icon: SidebarSimple },
  { dock: "right", label: "Dock to the right", icon: SidebarSimple, flip: true },
];

const CHROME_BTN = cn(
  "grid h-7 w-7 shrink-0 place-items-center rounded-md text-w-term-dim transition-colors hover:bg-w-term-raise hover:text-w-term-text disabled:pointer-events-none disabled:opacity-40 motion-reduce:transition-none",
  FOCUS,
  "focus-visible:ring-offset-0",
);
const PANE_BTN = "grid h-6 w-6 place-items-center rounded text-w-term-dim hover:bg-w-term-raise hover:text-w-term-text";
const NOTE_BTN = cn("inline-flex h-8 items-center gap-1.5 rounded-md border border-w-term-line bg-w-term-raise px-3 text-w-sm text-w-term-text hover:border-w-term-dim", FOCUS, "focus-visible:ring-offset-0");
const NO_ROOM = `Close a terminal first: there is room for ${MAX_PANES} at most.`;

type Drag = { kind: "pane" | "tab"; id: string };

function acceptDrop(e: React.DragEvent) {
  e.preventDefault();
  e.dataTransfer.dropEffect = "move";
}

function useDrag() {
  const [drag, setDragState] = useState<Drag | null>(null);
  const timer = useRef(0);
  const start = useCallback((next: Drag) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setDragState(next), 0);
  }, []);
  const end = useCallback(() => {
    window.clearTimeout(timer.current);
    setDragState(null);
  }, []);
  useEffect(() => {
    if (!drag) return;
    const finish = () => window.setTimeout(end, 0);
    const moved = (e: MouseEvent) => {
      if (e.buttons === 0) end();
    };
    window.addEventListener("dragend", finish, true);
    window.addEventListener("drop", finish);
    window.addEventListener("mousemove", moved);
    window.addEventListener("pointerdown", end);
    return () => {
      window.removeEventListener("dragend", finish, true);
      window.removeEventListener("drop", finish);
      window.removeEventListener("mousemove", moved);
      window.removeEventListener("pointerdown", end);
    };
  }, [drag, end]);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  return { drag, start, end };
}

export function TerminalPanel({
  meId,
  onClose,
  sideToggle,
  readerRef,
  front = true,
  request = null,
  notice,
  onTyped,
  place = { dock: "window", size: 0.38 },
  onPlace,
  command = null,
  source = { kind: "devices" },
  onQuote,
  onNote,
  fontScale = 1,
}: {
  meId: string | null;
  onClose: () => void;
  sideToggle?: ReactNode;
  readerRef?: MutableRefObject<TerminalReader | null>;
  front?: boolean;
  request?: TerminalRequest | null;
  notice?: ReactNode;
  onTyped?: (request: number, report: TypedReport | null) => void;
  place?: TerminalPlace;
  onPlace?: (place: TerminalPlace) => void;
  command?: TerminalCommand | null;
  source?: TerminalSource;
  onQuote?: ((quote: MessageQuote) => void) | null;
  onNote?: (note: string) => void;
  fontScale?: number;
}) {
  const workshop = source.kind === "workshop" ? source.sessionKey : null;
  const embed = source.kind === "embed" ? source : null;
  const storageKey = workshop ? workshopLayoutKey(workshop) : embed ? embedLayoutKey(embed.key) : layoutKey(meId);
  const docked = !workshop && !embed && place.dock !== "window";
  const profile = useMemo(() => keyProfile(isDesktopApp()), []);
  const hint = (action: LabelledAction) => shortcutLabel(action, profile);
  const [all, setAll] = useState<RunnerView[] | null>(null);
  const [local, setLocal] = useState<LocalCheck>(() => ({ state: workshop || !isDesktopApp() ? "known" : "checking", runner: null }));
  const localRunner = local.runner;
  const [deviceCap, setDeviceCap] = useState(DEVICE_TERMINALS);
  const [macCap, setMacCap] = useState(MAC_TERMINALS_PER_PLACE);
  const [listed, setListed] = useState<Set<string> | null>(workshop ? null : new Set());
  const readiness = useFeature("terminal");
  const restored = useRef<Set<string>>(new Set());
  const [layout, setLayoutState] = useState<TermLayout>(() => {
    const saved = typeof window === "undefined" ? null : loadLayout(window.localStorage, storageKey);
    if (saved) {
      restored.current = new Set(Object.values(saved.panes).filter((pane) => !embed || pane.native).map((pane) => pane.id));
      return saved;
    }
    if (embed) {
      const fresh = freshLayout(embed.device);
      const pane = Object.values(fresh.panes)[0];
      if (!embed.shell) return fresh;
      restored.current = new Set([pane.id]);
      return updatePane(fresh, pane.id, { native: embed.shell });
    }
    return freshLayout(null, place.dock, place.size);
  });
  const layoutRef = useRef(layout);
  layoutRef.current = layout;
  const ensureRef = useRef<(id: string) => void>(() => {});
  const setLayout = useCallback((change: (prev: TermLayout) => TermLayout) => {
    const next = change(layoutRef.current);
    if (next === layoutRef.current) return;
    layoutRef.current = next;
    for (const id of Object.keys(next.panes)) ensureRef.current(id);
    setLayoutState(next);
  }, []);
  const shells = useRef(new Map<string, Shell>());
  const [states, setStates] = useState<Record<string, ShellState>>({});
  const [quote, setQuote] = useState<{ pane: string; spot: { x: number; y: number } } | null>(null);
  const { drag, start: startDrag, end: endDrag } = useDrag();
  const [renaming, setRenaming] = useState<string | null>(null);
  const [typedNote, setTypedNoteState] = useState<string | null>(null);
  const onNoteRef = useRef(onNote);
  onNoteRef.current = onNote;
  const setTypedNote = useCallback((note: string | null) => {
    if (note && onNoteRef.current) onNoteRef.current(note);
    else setTypedNoteState(note);
  }, []);
  const autoTried = useRef(new Set<string>());
  const wantFocus = useRef(true);
  const [focusTick, setFocusTick] = useState(0);
  const requestFocus = useCallback(() => {
    wantFocus.current = true;
    setFocusTick((n) => n + 1);
  }, []);
  const onTypedRef = useRef(onTyped);
  onTypedRef.current = onTyped;
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  const devices = useMemo(() => (all === null ? null : all.filter((r) => r.status === "online" && (!meId || r.owner_user_id === meId))), [all, meId]);
  const localRef = useRef(localRunner);
  localRef.current = localRunner;
  const ready = targetsReady({ devices, local, listed });
  const slow = targetsSlow({ devices, local, listed });
  const readyRef = useRef(ready);
  readyRef.current = ready;
  const waiting = useRef<QueuedCommand[]>([]);
  const [queued, setQueued] = useState(0);
  const setWaiting = useCallback((next: QueuedCommand[]) => {
    waiting.current = next;
    setQueued(next.length);
  }, []);

  const loadDevices = useCallback(() => {
    if (workshop) return;
    worldApi
      .runners()
      .then((list) => setAll(list))
      .catch(() => setAll((prev) => prev ?? []));
  }, [workshop]);

  useEffect(() => {
    if (workshop) return;
    let gone = false;
    worldApi
      .terminalsPerDevice()
      .then((cap) => {
        if (!gone && typeof cap === "number" && cap > 0) setDeviceCap(cap);
      })
      .catch(() => {});
    return () => {
      gone = true;
    };
  }, [workshop]);

  useEffect(() => {
    if (workshop) {
      setAll([]);
      return;
    }
    loadDevices();
    window.addEventListener("world-reconnected", loadDevices);
    return () => window.removeEventListener("world-reconnected", loadDevices);
  }, [loadDevices, workshop]);

  useEffect(() => {
    if (workshop || !isDesktopApp()) return;
    return checkUntilAnswered(() => desktopApi.status(), {
      answer: (status) => {
        if (typeof status.terminals_per_place === "number") setMacCap(status.terminals_per_place);
        setLocal((prev) => nextLocalCheck(prev, { kind: "answer", runner: status.features?.includes("home_terminal") ? status.runner_id : null }));
      },
      slow: () => setLocal((prev) => nextLocalCheck(prev, { kind: "slow" })),
    });
  }, [workshop]);

  const deviceName = useCallback(
    (id: string | null) => {
      if (workshop) return WORKSHOP_PLACE;
      if (!id) return "a device";
      if (id === localRunner) return "this Mac";
      return all?.find((d) => d.id === id)?.name ?? "your device";
    },
    [all, localRunner, workshop],
  );
  const problemFor = useCallback(
    (current: TermLayout, device: string | null): string | null => {
      const cap = workshop ? macCap : device === null ? Infinity : device === localRunner ? macCap : deviceCap;
      const place = workshop ? "This Workshop" : device === localRunner ? "This Mac" : deviceName(device);
      return newPaneProblem(current, device, cap, place);
    },
    [workshop, macCap, deviceCap, localRunner, deviceName],
  );
  const deviceNameRef = useRef(deviceName);
  deviceNameRef.current = deviceName;

  const refresh = useCallback((id: string) => {
    const shell = shells.current.get(id);
    if (!shell) return;
    setStates((prev) => ({ ...prev, [id]: shell.state }));
  }, []);

  const runRef = useRef<(action: TerminalAction, pane?: string) => void>(() => {});
  const fontScaleRef = useRef(fontScale);
  fontScaleRef.current = fontScale;

  const ensureShell = useCallback(
    (id: string): Shell => {
      const known = shells.current.get(id);
      if (known) return known;
      const shell = new Shell(
        id,
        {
          changed: refresh,
          shortcut: (pane, action) => runRef.current(action, pane),
          focused: (pane) => {
            setLayout((prev) => focusPane(prev, pane));
          },
          selection: (pane, spot) => setQuote(spot ? { pane, spot } : null),
          native: (pane, native) => setLayout((prev) => updatePane(prev, pane, { native })),
        },
        profile,
      );
      shell.setScale(fontScaleRef.current);
      shells.current.set(id, shell);
      setStates((prev) => ({ ...prev, [id]: shell.state }));
      return shell;
    },
    [refresh, setLayout, profile],
  );

  ensureRef.current = ensureShell;

  const openOn = useCallback(
    (paneId: string, device: string | null, reattach: string | null = null) => {
      const shell = ensureShell(paneId);
      if (reattach === null && shell.phase === "elsewhere") reattach = layoutRef.current.panes[paneId]?.native ?? null;
      autoTried.current.add(paneId);
      restored.current.delete(paneId);
      if (workshop) {
        void shell.openLocal(workshopShell(workshop), reattach, null);
        return shell;
      }
      if (!device) return shell;
      setLayout((prev) => updatePane(prev, paneId, { device }));
      if (device === localRef.current) void shell.openLocal(homeShell, reattach, device);
      else void shell.openRemote(device);
      return shell;
    },
    [ensureShell, setLayout, workshop],
  );

  useEffect(() => {
    if (!workshop) return;
    let gone = false;
    void wb
      .termList(workshop)
      .catch(() => [])
      .then((terms) => {
        if (gone) return;
        setLayout((prev) =>
          adoptShells(
            prev,
            terms.map((t) => t.id),
            (id) => !autoTried.current.has(id),
          ),
        );
        setListed(new Set(terms.map((t) => t.id)));
      });
    return () => {
      gone = true;
    };
  }, [workshop, setLayout]);

  useEffect(() => {
    const live = new Set(Object.keys(layout.panes));
    for (const id of live) ensureShell(id);
    for (const [id, shell] of shells.current) {
      if (live.has(id)) continue;
      shell.dispose(true);
      shells.current.delete(id);
      autoTried.current.delete(id);
      setStates((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
    }
  }, [layout.panes, ensureShell]);

  useEffect(
    () => () => {
      for (const shell of shells.current.values()) shell.dispose(false);
      shells.current.clear();
      autoTried.current.clear();
    },
    [],
  );

  const fallback = useMemo(() => (devices ? defaultDevice({ devices: devices.map((d) => d.id), home: null, appRunner: localRunner }) : null), [devices, localRunner]);

  useEffect(() => {
    saveLayout(window.localStorage, storageKey, { ...layout, dock: place.dock, size: place.size });
  }, [layout, storageKey, place.dock, place.size]);

  const tab = activeTab(layout);
  const focus = tab?.focus ?? null;

  useEffect(() => {
    if (!focus || !wantFocus.current) return;
    wantFocus.current = false;
    let timer = 0;
    let tries = 0;
    const attempt = () => {
      const shell = shells.current.get(focus);
      if (shell?.host.isConnected) return shell.focus();
      tries += 1;
      if (tries < 40) timer = window.setTimeout(attempt, 25);
    };
    timer = window.setTimeout(attempt, 0);
    return () => window.clearTimeout(timer);
  }, [focus, layout.active, focusTick]);

  useEffect(() => {
    if (workshop || embed || !layout.tabs.length) return;
    publishTerminals(
      layout.tabs.map((t) => {
        const pane = layout.panes[t.focus];
        const state = states[t.focus];
        return { id: t.id, label: t.name ?? (state?.title || deviceName(pane?.device ?? null)), device: pane?.device ?? null, panes: panesIn(t.root).length };
      }),
    );
  }, [layout, states, deviceName, workshop]);

  const choices = useMemo(() => (workshop || (!ready && !slow) ? [] : deviceChoices(devices, localRunner)), [ready, slow, devices, localRunner, workshop]);

  const deviceForNew = useCallback(() => {
    const current = focusedPane(layoutRef.current);
    const shell = current ? shells.current.get(current.id) : null;
    return shell?.device ?? current?.device ?? fallback;
  }, [fallback]);

  const place_ = useCallback(
    (pane: TermPane, how: "tab" | SplitDir, target: string | null): boolean => {
      const problem = problemFor(layoutRef.current, pane.device);
      if (problem) {
        setTypedNote(problem);
        return false;
      }
      const next = placePane(layoutRef.current, pane, how, target);
      if (!next) {
        setTypedNote(NO_ROOM);
        return false;
      }
      setLayout(() => next);
      return true;
    },
    [setLayout, setTypedNote, problemFor],
  );

  const closeMany = useCallback(
    (paneIds: string[]) => {
      for (const id of paneIds) shells.current.get(id)?.kill();
      const next = paneIds.reduce((acc, id) => closePane(acc, id), layoutRef.current);
      requestFocus();
      setLayout(() => next);
      if (next.tabs.length) return;
      saveLayout(window.localStorage, storageKey, null);
      if (!workshop && !embed) publishTerminals([]);
      onCloseRef.current();
    },
    [storageKey, setLayout, requestFocus, workshop, embed],
  );
  const closeOne = useCallback((paneId: string) => closeMany([paneId]), [closeMany]);

  const run = useCallback(
    (action: TerminalAction, paneId?: string, device?: string) => {
      if (embed && (action === "new_tab" || action === "tab_next" || action === "tab_prev")) return;
      const current = layoutRef.current;
      const target = paneId ?? activeTab(current)?.focus ?? null;
      requestFocus();
      if (!readyRef.current && (action === "new_tab" || action === "split_right" || action === "split_down")) {
        setWaiting(enqueue(waiting.current, { id: Date.now(), action, device, pane: paneId, inside: true }));
        return;
      }
      switch (action) {
        case "new_tab":
          place_(newPane(device ?? deviceForNew()), "tab", target);
          return;
        case "split_right":
        case "split_down":
          if (target) place_(newPane(device ?? deviceForNew()), action === "split_right" ? "row" : "column", target);
          return;
        case "close":
          if (target) closeOne(target);
          return;
        case "focus_next":
          setLayout((prev) => cycleFocus(prev, 1));
          return;
        case "focus_prev":
          setLayout((prev) => cycleFocus(prev, -1));
          return;
        case "tab_next":
          setLayout((prev) => cycleTab(prev, 1));
          return;
        case "tab_prev":
          setLayout((prev) => cycleTab(prev, -1));
          return;
        case "focus_left":
          setLayout((prev) => focusToward(prev, "left"));
          return;
        case "focus_right":
          setLayout((prev) => focusToward(prev, "right"));
          return;
        case "focus_up":
          setLayout((prev) => focusToward(prev, "up"));
          return;
        case "focus_down":
          setLayout((prev) => focusToward(prev, "down"));
          return;
      }
    },
    [closeOne, deviceForNew, setLayout, place_, requestFocus, setWaiting, embed],
  );
  runRef.current = run;

  const handledCommands = useRef(new WeakSet<QueuedCommand>());
  useEffect(() => {
    if (command && !handledCommands.current.has(command) && !waiting.current.includes(command)) {
      const next = enqueue(waiting.current, command);
      if (next === waiting.current) handledCommands.current.add(command);
      waiting.current = next;
    }
    const pending = waiting.current;
    waiting.current = [];
    const still: QueuedCommand[] = [];
    for (const next of pending) {
      const device = next.device ?? null;
      const blank = focusedPane(layoutRef.current);
      const blankFree = blankPane(blank, (blank ? shells.current.get(blank.id)?.phase ?? "idle" : "idle") === "idle");
      const paneGone = !!next.pane && !layoutRef.current.panes[next.pane];
      const plan = planCommand({ ready, reuse: !next.inside, action: next.action, device, blankFree, paneGone });
      if (plan === "wait") {
        still.push(next);
        continue;
      }
      handledCommands.current.add(next);
      if (plan === "drop") continue;
      if (plan === "place" && next.action !== "focus") {
        run(next.action, paneGone ? undefined : next.pane, next.device);
        continue;
      }
      requestFocus();
      if (next.tab) setLayout((prev) => selectTab(prev, next.tab ?? ""));
      if (plan !== "open_blank" || !blank || !device) continue;
      const problem = problemFor(layoutRef.current, device);
      if (problem) setTypedNote(problem);
      else openOn(blank.id, device);
    }
    setWaiting([...still, ...waiting.current]);
  }, [command, ready, run, setLayout, requestFocus, openOn, problemFor, setTypedNote, setWaiting]);

  useEffect(() => {
    if (!ready || devices === null || listed === null) return;
    const online = new Set(devices.map((d) => d.id));
    const tab = activeTab(layout);
    for (const pane of Object.values(layout.panes)) {
      if (autoTried.current.has(pane.id)) continue;
      const shell = shells.current.get(pane.id);
      if (!shell || shell.phase !== "idle") continue;
      if (workshop) {
        openOn(pane.id, null, pane.native && listed.has(pane.native) ? pane.native : null);
        continue;
      }
      const opensNew = !embed || embed.startsShell;
      if (restored.current.has(pane.id)) {
        const plan = planRestored({ ready, pane, focused: pane.id === tab?.focus, online, localRunner });
        if (plan === "reattach") openOn(pane.id, localRunner, pane.native);
        else if (plan === "open" && opensNew) openOn(pane.id, pane.device);
        else autoTried.current.add(pane.id);
        continue;
      }
      const device = pane.device ?? fallback;
      if (opensNew && device && (online.has(device) || device === localRunner)) openOn(pane.id, device);
      else autoTried.current.add(pane.id);
    }
  }, [ready, layout, devices, localRunner, fallback, openOn, listed, workshop, embed]);

  useEffect(() => {
    for (const shell of shells.current.values()) shell.setScale(fontScale);
  }, [fontScale, layout.panes]);

  const handled = useRef<number | null>(null);
  useEffect(() => {
    if (!request || handled.current === request.id) return;
    if (!ready) return;
    handled.current = request.id;
    setTypedNote(null);
    const current = focusedPane(layoutRef.current);
    const shell = current ? ensureShell(current.id) : null;
    const target = workshop ? "workshop" : request.runnerId ?? (shell?.busy ? shell.device : null) ?? fallback;
    const plan = planRequest({ phase: shell?.phase ?? "idle", current: workshop ? "workshop" : shell?.device ?? null, target });
    const finish = (pane: Shell | null) => {
      if (!pane) {
        onTypedRef.current?.(request.id, null);
        return;
      }
      void pane.type(request.command).then((result) => {
        const report = result ? typedReport(result, deviceNameRef.current(pane.device)) : null;
        if (report && report.result !== "typed") setTypedNote(report.note);
        else if (!workshop) setTypedNote(report?.note ?? null);
        onTypedRef.current?.(request.id, report);
      });
    };
    if (plan === "none" || !target) {
      setTypedNote("No device is online to type it on. Pair or wake one in Devices.");
      onTypedRef.current?.(request.id, null);
      return;
    }
    requestFocus();
    if (plan === "type" || plan === "queue") return finish(shell);
    if (plan === "open" && current) return finish(openOn(current.id, workshop ? null : target));
    const pane = newPane(workshop ? null : target);
    if (!place_(pane, "tab", current?.id ?? null)) {
      onTypedRef.current?.(request.id, null);
      return;
    }
    finish(openOn(pane.id, workshop ? null : target));
  }, [request, ready, fallback, ensureShell, openOn, place_, requestFocus, setTypedNote, workshop]);

  const readFocused = useCallback((selectionOnly: boolean): TerminalQuote | null => {
    const id = activeTab(layoutRef.current)?.focus;
    const shell = id ? shells.current.get(id) : null;
    return shell ? shell.read(selectionOnly, deviceNameRef.current(shell.device)) : null;
  }, []);

  useEffect(() => {
    if (!readerRef) return;
    readerRef.current = () => readFocused(false);
    return () => {
      readerRef.current = null;
    };
  }, [readerRef, readFocused]);

  const quoting = workshop ? onQuote ?? null : onQuote ?? ((picked: MessageQuote) => sendQuote(MAIN_TARGET, picked));
  const quoteSelection = () => {
    const shell = quote ? shells.current.get(quote.pane) : null;
    setQuote(null);
    if (!shell || !quoting) return;
    const picked = shell.read(true, deviceName(shell.device));
    if (!picked) return;
    shell.term.clearSelection();
    quoting(picked);
  };

  useEffect(() => {
    setQuote(null);
  }, [layout.active]);

  useEffect(() => {
    if (workshop || embed || docked || !front) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || escapeTaken(e)) return;
      if ((e.target as HTMLElement | null)?.closest?.(".xterm")) return;
      onCloseRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [docked, front, workshop]);

  const focusedState = focus ? states[focus] : undefined;
  const focusedDevice = focusedState?.device ?? (focus ? layout.panes[focus]?.device ?? null : null);
  const blocked = !workshop && ready && devices !== null && devices.length === 0 && !localRunner && readiness && readiness.state !== "ready" ? readiness : null;
  const paneTotal = Object.keys(layout.panes).length;
  const online = new Set([...(devices ?? []).map((d) => d.id), ...(localRunner ? [localRunner] : [])]);
  const deviceIds = new Set((devices ?? []).map((d) => d.id));
  const restoredPlan = (paneId: string) => {
    const pane = layout.panes[paneId];
    return pane && restored.current.has(paneId) ? planRestored({ ready, pane, focused: paneId === focus, online: deviceIds, localRunner }) : null;
  };
  const waitingPanes = tab ? panesIn(tab.root).map((id) => layout.panes[id]).filter((p) => p && p.device && online.has(p.device) && states[p.id]?.phase === "idle" && restored.current.has(p.id) && restoredPlan(p.id) !== "wait") : [];
  const focusedBlank = focus ? layout.panes[focus] ?? null : null;
  const opening = queued ? queuedOpens(waiting.current, { blankFree: blankPane(focusedBlank, (focus ? states[focus]?.phase ?? "idle" : "idle") === "idle"), panes: new Set(Object.keys(layout.panes)) }) : 0;
  const queuedNote = workshop ? null : waitingNote({ devices, local, listed }, opening);
  const newBlock = problemFor(layout, deviceForNew());
  const blockedBtn = (blocked: string | null) => (blocked ? "opacity-40 hover:bg-transparent hover:text-w-term-dim" : undefined);

  const status = queuedNote ? (
    <Shimmer>{slow ? "Still checking this Mac" : "Looking for devices"}</Shimmer>
  ) : !focusedState ? null : focusedState.phase === "live" ? (
    <span className="flex items-center gap-1.5">
      <ConnectionMark state="ok" size={12} />
      {paneTotal > 1 ? `Live on ${deviceName(focusedDevice)}, ${paneTotal} terminals open` : `Live on ${deviceName(focusedDevice)}`}
    </span>
  ) : focusedState.phase === "connecting" || focusedState.phase === "starting" ? (
    <Shimmer>Connecting to {deviceName(focusedDevice)}</Shimmer>
  ) : focusedState.phase === "ended" ? (
    focusedState.detail ? closeLabel(focusedState.detail) : "Session ended"
  ) : focusedState.phase === "elsewhere" ? (
    "Showing in another place"
  ) : focusedState.phase === "error" ? (
    <span className="text-w-danger">{focusedState.detail}</span>
  ) : slow ? (
    <Shimmer>Still checking this Mac</Shimmer>
  ) : !ready || devices === null ? (
    <Shimmer>Looking for devices</Shimmer>
  ) : devices.length === 0 && !localRunner ? (
    "No devices online"
  ) : (
    "Open a shell on one of your devices"
  );

  const cancelQueued = () => {
    for (const queuedCommand of waiting.current) handledCommands.current.add(queuedCommand);
    setWaiting([]);
  };

  const onPanelKey = (e: React.KeyboardEvent) => {
    const el = e.target as HTMLElement;
    if (el.closest(".xterm") || el.matches("input, textarea")) return;
    const action = terminalShortcut(e.nativeEvent, profile);
    if (!action) return;
    e.preventDefault();
    e.stopPropagation();
    run(action);
  };

  const dropOnPane = (target: string, zone: Zone) => {
    const dragging = drag;
    endDrag();
    if (!dragging) return;
    requestFocus();
    if (dragging.kind === "pane") setLayout((prev) => movePane(prev, dragging.id, target, zone));
    else setLayout((prev) => moveTab(prev, dragging.id, target, zone));
  };

  const dropOnTab = (before: string | null) => {
    const dragging = drag;
    endDrag();
    if (!dragging) return;
    if (dragging.kind === "tab") setLayout((prev) => reorderTab(prev, dragging.id, before));
    else setLayout((prev) => paneToTab(prev, dragging.id, before));
  };

  const tabLabel = (t: TermTab) => t.name ?? (states[t.focus]?.title || deviceName(layout.panes[t.focus]?.device ?? states[t.focus]?.device ?? null));

  const strip = (
    <div
      role="tablist"
      aria-label="Terminals"
      className="flex h-10 shrink-0 items-center gap-1 border-b border-w-term-line bg-w-term-bar px-2"
      onDragEnter={(e) => drag && acceptDrop(e)}
      onDragOver={(e) => drag && acceptDrop(e)}
      onDrop={(e) => {
        e.preventDefault();
        dropOnTab(null);
      }}
    >
      <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto [scrollbar-width:none]">
        {layout.tabs.map((t) => (
          <TabButton
            key={t.id}
            tab={t}
            label={tabLabel(t)}
            active={t.id === layout.active}
            panes={panesIn(t.root).length}
            renaming={renaming === t.id}
            dragging={drag?.kind === "tab" && drag.id === t.id}
            onSelect={() => {
              requestFocus();
              setLayout((prev) => selectTab(prev, t.id));
            }}
            onRename={() => setRenaming(t.id)}
            onRenamed={(name) => {
              setRenaming(null);
              if (name !== null) setLayout((prev) => renameTab(prev, t.id, name));
              requestFocus();
            }}
            onClose={() => closeMany(panesIn(t.root))}
            onDragStart={(e) => {
              startDrag({ kind: "tab", id: t.id });
              const pane = layout.panes[t.focus];
              const device = states[t.focus]?.device ?? pane?.device ?? null;
              if (device) e.dataTransfer.setData(EMBED_DRAG, embedDragData({ kind: "terminal", target: terminalTarget(device, device === localRunner ? pane?.native ?? null : null) }, `Terminal on ${tabLabel(t)}`));
            }}
            onDragEnd={endDrag}
            onDropBefore={() => dropOnTab(t.id)}
            droppable={!!drag}
          />
        ))}
        <button type="button" onClick={() => run("new_tab")} aria-disabled={!!newBlock} aria-label="New terminal" title={newBlock ?? `New terminal (${hint("new_tab")})`} className={cn(CHROME_BTN, blockedBtn(newBlock))}>
          <Plus size={14} weight="bold" />
        </button>
        {ready && choices.length > 1 && <NewOnMenu choices={choices} canSplit={!!focus} blocked={(device) => problemFor(layout, device)} hint={hint} onPick={(action, device) => run(action, undefined, device)} />}
      </div>
      <div className="flex shrink-0 items-center gap-0.5">
        <button type="button" onClick={() => run("split_right")} disabled={!focus} aria-disabled={!!newBlock} aria-label="Split right" title={newBlock ?? `Split right (${hint("split_right")})`} className={cn(CHROME_BTN, blockedBtn(newBlock))}>
          <SquareSplitHorizontal size={15} weight="bold" />
        </button>
        <button type="button" onClick={() => run("split_down")} disabled={!focus} aria-disabled={!!newBlock} aria-label="Split down" title={newBlock ?? `Split down (${hint("split_down")})`} className={cn(CHROME_BTN, blockedBtn(newBlock))}>
          <SquareSplitVertical size={15} weight="bold" />
        </button>
        {focus && (workshop || docked) && shells.current.get(focus)?.busy && (
          <button type="button" onClick={() => shells.current.get(focus)?.kill()} aria-label="End the shell" title="End the shell in the focused terminal" className={cn(CHROME_BTN, "hover:text-w-term-red")}>
            <Skull size={15} weight="bold" />
          </button>
        )}
        {onPlace && !workshop && (
          <div role="group" aria-label="Where the terminal sits" className="ml-1 flex items-center gap-0.5 border-l border-w-term-line pl-1.5">
            {DOCK_CHOICES.map((choice) => {
              const on = place.dock === choice.dock;
              const Icon = choice.icon;
              return (
                <button
                  key={choice.dock}
                  type="button"
                  aria-pressed={on}
                  aria-label={choice.label}
                  title={choice.label}
                  onClick={() => {
                    requestFocus();
                    onPlace({ dock: choice.dock, size: place.size });
                  }}
                  className={cn(CHROME_BTN, on && "bg-w-term-raise text-w-term-focus")}
                >
                  <Icon size={15} weight={on ? "fill" : "bold"} className={choice.flip ? "-scale-x-100" : undefined} />
                </button>
              );
            })}
          </div>
        )}
        {workshop ? (
          <button type="button" onClick={onClose} aria-label="Hide terminal panel" title="Hide the terminal, your shells keep running" className={cn(CHROME_BTN, "ml-1")}>
            <CaretDown size={15} weight="bold" />
          </button>
        ) : (
          docked && (
            <button type="button" onClick={onClose} aria-label="Hide terminal" title="Hide terminal, your shells keep their place" className={cn(CHROME_BTN, "ml-1")}>
              <X size={15} weight="bold" />
            </button>
          )
        )}
      </div>
    </div>
  );

  const flat = workshop || docked || !!embed;

  return (
    <div
      role={flat ? "region" : "dialog"}
      aria-modal={flat ? undefined : true}
      aria-label="Terminal"
      onKeyDown={onPanelKey}
      className={cn(flat ? "bg-w-term-bg" : PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}
    >
      {!flat && (
        <WindowHeader
          title="Terminal"
          subtitle={status}
          onClose={onClose}
          actions={
            (sideToggle || focusedState?.phase === "live" || focusedState?.phase === "connecting" || focusedState?.phase === "starting") && (
              <>
                {sideToggle}
                {focus && shells.current.get(focus)?.busy && (
                  <button type="button" onClick={() => shells.current.get(focus)?.kill()} title="End the shell in the focused terminal" className={BTN_SECONDARY_DANGER}>
                    <Skull size={14} weight="bold" />
                    <span className="hidden sm:inline">Kill</span>
                  </button>
                )}
              </>
            )
          }
        />
      )}
      {(typedNote || notice || queuedNote) && (
        <div role="status" className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b-2 border-dashed border-w-line2 bg-w-sand px-4 py-2 text-w-sm text-w-text2 sm:px-5">
          {queuedNote && (
            <span className="flex min-w-0 flex-1 basis-full items-center gap-3">
              <Shimmer className="min-w-0 flex-1">{queuedNote}</Shimmer>
              <button type="button" onClick={cancelQueued} title="Do not open them" className="h-8 shrink-0 rounded-lg px-2.5 text-w-xs font-semibold text-w-muted hover:bg-w-paper hover:text-w-text">
                Cancel
              </button>
            </span>
          )}
          {typedNote && <span className="min-w-0 flex-1">{typedNote}</span>}
          {notice}
          {typedNote && (
            <button type="button" onClick={() => setTypedNote(null)} aria-label="Dismiss" title="Dismiss" className="grid h-8 w-8 place-items-center rounded-lg text-w-muted hover:bg-w-paper hover:text-w-text">
              <X size={14} weight="bold" />
            </button>
          )}
        </div>
      )}
      <div className="flex min-h-0 flex-1 flex-col bg-w-term-bg">
        {!embed && strip}
        <div className="relative min-h-0 flex-1">
          {blocked ? (
            <div className="absolute inset-0 flex items-center justify-center p-6">
              <SetupHint feature={blocked} tone="dark" className="max-w-lg" />
            </div>
          ) : (
            tab && (
              <Tree
                node={tab.root}
                path=""
                tab={tab}
                panes={layout.panes}
                shells={shells.current}
                states={states}
                focus={tab.focus}
                many={panesIn(tab.root).length > 1}
                drag={drag}
                workshop={!!workshop}
                ready={ready}
                slow={slow}
                devices={devices}
                localRunner={localRunner}
                choices={choices}
                restored={restored.current}
                restoredPlan={restoredPlan}
                fontScale={fontScale}
                deviceName={deviceName}
                onFocus={(id) => {
                  requestFocus();
                  setLayout((prev) => focusPane(prev, id));
                }}
                onRatio={(path, ratio) => setLayout((prev) => setRatio(prev, tab.id, path, ratio))}
                onDragPane={(id) => startDrag({ kind: "pane", id })}
                onDragEnd={endDrag}
                onDrop={dropOnPane}
                onOpen={(id, device) => {
                  requestFocus();
                  setLayout((prev) => focusPane(prev, id));
                  openOn(id, device);
                }}
                onRetry={loadDevices}
                blocked={(device) => problemFor(layout, device)}
                waiting={waitingPanes.length}
                onReconnectAll={() => {
                  for (const pane of waitingPanes) if (pane.device) openOn(pane.id, pane.device);
                  requestFocus();
                }}
                onSplit={(id) => run("split_right", id)}
                onClosePane={(id) => closeOne(id)}
              />
            )
          )}
          <QuoteButton spot={front && quote && quoting ? quote.spot : null} onQuote={quoteSelection} />
        </div>
      </div>
    </div>
  );
}

function NewOnMenu({
  choices,
  canSplit,
  blocked,
  hint,
  onPick,
}: {
  choices: TerminalDevice[];
  canSplit: boolean;
  blocked: (device: string) => string | null;
  hint: (action: LabelledAction) => string;
  onPick: (action: "new_tab" | "split_right" | "split_down", device: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const pick = (action: "new_tab" | "split_right" | "split_down", device: string) => {
    setOpen(false);
    onPick(action, device);
  };
  return (
    <PopoverPrimitive.Root open={open} onOpenChange={setOpen}>
      <PopoverPrimitive.Trigger asChild>
        <button type="button" aria-label="New terminal on a device" title="New terminal on a device" className={cn(CHROME_BTN, "-ml-1 w-5")}>
          <CaretDown size={11} weight="bold" />
        </button>
      </PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content side="bottom" align="start" sideOffset={6} collisionPadding={12} className={cn(MENU, "z-[80] w-72 animate-w-modal-in data-[state=closed]:animate-w-modal-out")}>
          <p className="px-2.5 pb-1.5 pt-1 text-w-2xs text-w-muted">New terminal on</p>
          {choices.map((d) => {
            const full = blocked(d.id);
            return (
              <div key={d.id} className="flex items-center gap-0.5" title={full ?? undefined}>
                <button type="button" aria-disabled={!!full} onClick={() => pick("new_tab", d.id)} className={cn(MENU_ITEM, "min-w-0 flex-1 pl-2.5", full && "opacity-40")}>
                  <Monitor size={15} weight="duotone" className="shrink-0 text-w-muted" />
                  <span className="truncate">{d.name}</span>
                </button>
                {canSplit && (
                  <>
                    <button type="button" aria-disabled={!!full} aria-label={`Split right on ${d.name}`} title={full ?? `Split right on ${d.name} (${hint("split_right")})`} onClick={() => pick("split_right", d.id)} className={cn("grid h-8 w-8 shrink-0 place-items-center rounded-lg text-w-muted hover:bg-w-sand hover:text-w-strong", full && "opacity-40")}>
                      <SquareSplitHorizontal size={15} weight="bold" />
                    </button>
                    <button type="button" aria-disabled={!!full} aria-label={`Split down on ${d.name}`} title={full ?? `Split down on ${d.name} (${hint("split_down")})`} onClick={() => pick("split_down", d.id)} className={cn("grid h-8 w-8 shrink-0 place-items-center rounded-lg text-w-muted hover:bg-w-sand hover:text-w-strong", full && "opacity-40")}>
                      <SquareSplitVertical size={15} weight="bold" />
                    </button>
                  </>
                )}
              </div>
            );
          })}
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}

function TabButton({
  tab,
  label,
  active,
  panes,
  renaming,
  dragging,
  droppable,
  onSelect,
  onRename,
  onRenamed,
  onClose,
  onDragStart,
  onDragEnd,
  onDropBefore,
}: {
  tab: TermTab;
  label: string;
  active: boolean;
  panes: number;
  renaming: boolean;
  dragging: boolean;
  droppable: boolean;
  onSelect: () => void;
  onRename: () => void;
  onRenamed: (name: string | null) => void;
  onClose: () => void;
  onDragStart: (e: React.DragEvent) => void;
  onDragEnd: () => void;
  onDropBefore: () => void;
}) {
  const [over, setOver] = useState(false);
  const field = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (renaming) field.current?.select();
  }, [renaming]);
  return (
    <div
      role="presentation"
      onDragEnter={(e) => {
        if (!droppable) return;
        acceptDrop(e);
        e.stopPropagation();
        setOver(true);
      }}
      onDragOver={(e) => {
        if (!droppable) return;
        acceptDrop(e);
        e.stopPropagation();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        e.stopPropagation();
        setOver(false);
        onDropBefore();
      }}
      className={cn(
        "group/tab relative flex h-7 min-w-0 max-w-[14rem] shrink-0 items-center rounded-md text-w-sm transition-colors motion-reduce:transition-none",
        active ? "bg-w-term-raise text-w-term-text" : "text-w-term-dim hover:bg-w-term-raise/60 hover:text-w-term-text",
        dragging && "opacity-40",
        over && "shadow-[inset_2px_0_0_rgb(var(--w-term-focus))]",
      )}
    >
      {renaming ? (
        <input
          ref={field}
          defaultValue={tab.name ?? label}
          aria-label="Terminal name"
          maxLength={40}
          onKeyDown={(e) => {
            if (e.key === "Enter") onRenamed(e.currentTarget.value);
            else if (e.key === "Escape") {
              takeEscape(e);
              onRenamed(null);
            }
          }}
          onBlur={(e) => onRenamed(e.currentTarget.value)}
          className="h-7 w-40 rounded-md border border-w-term-focus/60 bg-w-term-bg px-2 text-w-sm text-w-term-text outline-none"
        />
      ) : (
        <button
          type="button"
          role="tab"
          aria-selected={active}
          draggable
          onDragStart={(e) => {
            e.dataTransfer.effectAllowed = "copyMove";
            e.dataTransfer.setData("text/plain", label);
            onDragStart(e);
          }}
          onDragEnd={onDragEnd}
          onClick={onSelect}
          onDoubleClick={onRename}
          title={`${label}. Double-click to rename, drag to move`}
          className={cn("flex h-7 min-w-0 items-center gap-1.5 rounded-md pl-2.5 pr-6", FOCUS, "focus-visible:ring-offset-0")}
        >
          <TerminalWindow size={13} weight={active ? "fill" : "regular"} className={cn("shrink-0", active && "text-w-term-focus")} />
          <span className="truncate">{label}</span>
          {panes > 1 && <span className="shrink-0 text-w-2xs text-w-term-dim">{panes}</span>}
        </button>
      )}
      {!renaming && (
        <button
          type="button"
          onClick={onClose}
          aria-label={`Close ${label}`}
          title="Close this tab and its shells"
          className={cn("absolute right-1 grid h-5 w-5 place-items-center rounded text-w-term-dim hover:bg-w-term-line hover:text-w-term-text", active ? "opacity-100" : "opacity-0 group-hover/tab:opacity-100 focus-visible:opacity-100")}
        >
          <X size={11} weight="bold" />
        </button>
      )}
    </div>
  );
}

type TreeProps = {
  tab: TermTab;
  panes: Record<string, TermPane>;
  shells: Map<string, Shell>;
  states: Record<string, ShellState>;
  focus: string;
  many: boolean;
  drag: Drag | null;
  workshop: boolean;
  ready: boolean;
  slow: boolean;
  devices: RunnerView[] | null;
  localRunner: string | null;
  choices: TerminalDevice[];
  restored: Set<string>;
  restoredPlan: (id: string) => RestoredPlan | null;
  fontScale: number;
  deviceName: (id: string | null) => string;
  onFocus: (id: string) => void;
  onRatio: (path: string, ratio: number) => void;
  onDragPane: (id: string) => void;
  onDragEnd: () => void;
  onDrop: (target: string, zone: Zone) => void;
  onOpen: (id: string, device: string | null) => void;
  onRetry: () => void;
  blocked: (device: string) => string | null;
  waiting: number;
  onReconnectAll: () => void;
  onSplit: (id: string) => void;
  onClosePane: (id: string) => void;
};

function Tree({ node, path, ...props }: TreeProps & { node: LayoutNode; path: string }) {
  const box = useRef<HTMLDivElement>(null);
  if (node.kind === "pane") return <PaneView id={node.id} {...props} />;
  const row = node.dir === "row";
  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const el = box.current;
    if (!el) return;
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const rect = el.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      const ratio = row ? (ev.clientX - rect.left) / rect.width : (ev.clientY - rect.top) / rect.height;
      props.onRatio(path, ratio);
    };
    const end = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };
  const nudge = (e: React.KeyboardEvent) => {
    const step = e.key === (row ? "ArrowLeft" : "ArrowUp") ? -0.05 : e.key === (row ? "ArrowRight" : "ArrowDown") ? 0.05 : 0;
    if (!step) return;
    e.preventDefault();
    props.onRatio(path, node.ratio + step);
  };
  return (
    <div ref={box} className={cn("flex h-full min-h-0 w-full min-w-0", row ? "flex-row" : "flex-col")}>
      <div className="relative min-h-0 min-w-0" style={{ flex: `${node.ratio} 1 0%` }}>
        <Tree node={node.a} path={`${path}a`} {...props} />
      </div>
      <div
        role="separator"
        tabIndex={0}
        aria-orientation={row ? "vertical" : "horizontal"}
        aria-valuenow={Math.round(node.ratio * 100)}
        aria-label="Resize terminals"
        onPointerDown={startDrag}
        onKeyDown={nudge}
        onDoubleClick={() => props.onRatio(path, 0.5)}
        className={cn(
          "group/sep relative z-10 shrink-0 outline-none",
          row ? "w-px cursor-col-resize bg-w-term-line before:absolute before:inset-y-0 before:-left-1.5 before:w-3.5" : "h-px cursor-row-resize bg-w-term-line before:absolute before:inset-x-0 before:-top-1.5 before:h-3.5",
          "hover:bg-w-term-focus/70 focus-visible:bg-w-term-focus",
        )}
      />
      <div className="relative min-h-0 min-w-0" style={{ flex: `${1 - node.ratio} 1 0%` }}>
        <Tree node={node.b} path={`${path}b`} {...props} />
      </div>
    </div>
  );
}

function PaneView({ id, panes, shells, states, focus, many, drag, workshop, ready, slow, devices, localRunner, choices, restored, restoredPlan, fontScale, deviceName, onFocus, onDragPane, onDragEnd, onDrop, onOpen, onRetry, blocked, waiting, onReconnectAll, onSplit, onClosePane }: TreeProps & { id: string }) {
  const slot = useRef<HTMLDivElement>(null);
  const shell = shells.get(id);
  const state = states[id];
  const [zone, setZone] = useState<Zone | null>(null);
  useLayoutEffect(() => {
    const here = slot.current;
    if (!here || !shell) return;
    here.appendChild(shell.host);
    const timer = window.setTimeout(() => shell.fit(), 0);
    return () => {
      window.clearTimeout(timer);
      if (shell.host.parentElement === here) here.removeChild(shell.host);
    };
  }, [shell]);
  useEffect(() => {
    if (!drag) setZone(null);
  }, [drag]);
  const focused = id === focus;
  const device = state?.device ?? panes[id]?.device ?? null;
  const online = devices?.filter((d) => d.status === "online") ?? [];
  const known = !device || device === localRunner || online.some((d) => d.id === device);
  const phase = state?.phase ?? "idle";
  const others = otherDevices(choices, device);

  const picker = (label: string, list: TerminalDevice[] = choices) =>
    !ready && !slow ? (
      <Shimmer>Looking for devices</Shimmer>
    ) : list.length === 0 ? (
      <p className="text-w-sm text-w-term-dim">No devices are online. Pair or wake one in Devices.</p>
    ) : (
      <div className="flex flex-col items-center gap-2">
        <p className="text-w-sm text-w-term-dim">{label}</p>
        <div className="flex max-w-md flex-wrap justify-center gap-1.5">
          {list.map((d) => {
            const full = blocked(d.id);
            return (
            <button key={d.id} type="button" aria-disabled={!!full} title={full ?? undefined} onClick={() => !full && onOpen(id, d.id)} className={cn(NOTE_BTN, full && "opacity-40")}>
              <Monitor size={14} weight="duotone" className="text-w-term-dim" />
              {d.name}
            </button>
            );
          })}
        </div>
      </div>
    );

  let cover: ReactNode = null;
  if (phase === "idle" && slow && restoredPlan(id) === "wait") cover = <Shimmer>Checking this Mac to reconnect your shell</Shimmer>;
  else if (phase === "idle" && !ready && !slow) cover = <Shimmer>{workshop ? "Looking for running shells" : "Looking for devices"}</Shimmer>;
  else if (phase === "idle" && workshop)
    cover = (
      <div className="flex flex-col items-center gap-3">
        <p className="text-w-sm text-w-term-dim">No shell running here.</p>
        <button type="button" onClick={() => onOpen(id, null)} className={NOTE_BTN}>
          <ArrowClockwise size={14} weight="bold" />
          New shell
        </button>
      </div>
    );
  else if (phase === "idle") {
    if (device && !known)
      cover = (
        <div className="flex flex-col items-center gap-3">
          <p className="text-w-sm text-w-term-text">{deviceName(device)} is offline.</p>
          <p className="max-w-xs text-w-xs text-w-term-dim">Wake it or open its app, then try again. You can also open a shell on another device.</p>
          <button type="button" onClick={onRetry} className={NOTE_BTN}>
            <ArrowClockwise size={14} weight="bold" />
            Try again
          </button>
          {others.length > 0 && picker("Or open one on", others)}
        </div>
      );
    else if (device)
      cover = (
        <div className="flex flex-col items-center gap-3">
          <p className="text-w-sm text-w-term-dim">{restored.has(id) ? `The shell on ${deviceName(device)} closed with the terminal.` : "No shell running here."}</p>
          <div className="flex flex-wrap justify-center gap-1.5">
            <button type="button" onClick={() => onOpen(id, device)} className={NOTE_BTN}>
              <ArrowClockwise size={14} weight="bold" />
              {restored.has(id) ? "Reconnect" : `New shell on ${deviceName(device)}`}
            </button>
            {restored.has(id) && waiting > 1 && (
              <button type="button" onClick={onReconnectAll} className={NOTE_BTN}>
                Reconnect all {waiting}
              </button>
            )}
          </div>
          {others.length > 0 && picker("Or open one on", others)}
        </div>
      );
    else cover = picker(slow ? "Still checking this Mac. Open a shell on" : "Open a shell on");
  } else if (phase === "elsewhere")
    cover = (
      <div className="flex flex-col items-center gap-3">
        <p className="max-w-xs text-center text-w-sm text-w-term-dim">This shell is showing in another place, so it is paused here.</p>
        <button type="button" onClick={() => onOpen(id, device)} className={NOTE_BTN}>
          <ArrowClockwise size={14} weight="bold" />
          Open here
        </button>
      </div>
    );
  else if (phase === "error")
    cover = (
      <div className="flex flex-col items-center gap-3">
        <p className="max-w-sm text-w-sm text-w-term-red">{state?.detail || "Could not open a shell."}</p>
        {(device || workshop) && (
          <button type="button" onClick={() => onOpen(id, device)} className={NOTE_BTN}>
            <ArrowClockwise size={14} weight="bold" />
            Try again
          </button>
        )}
      </div>
    );

  return (
    <div
      className="group/pane relative h-full w-full min-w-0"
      onPointerDown={() => {
        if (!focused) onFocus(id);
      }}
    >
      <div className={cn("absolute inset-0 py-2 pl-3 pr-1", cover && "invisible")}>
        <div ref={slot} className="h-full w-full origin-top-left" style={fontScale === 1 ? undefined : { width: `${fontScale * 100}%`, height: `${fontScale * 100}%`, transform: `scale(${1 / fontScale})` }} />
      </div>
      {many && !focused && <div aria-hidden className="pointer-events-none absolute inset-0 z-10 bg-w-term-bg/40" />}
      {many && focused && <div aria-hidden className="pointer-events-none absolute inset-0 z-10 shadow-[inset_0_0_0_1px_rgb(var(--w-term-focus)/0.45)]" />}
      {(phase === "starting" || phase === "connecting") && (
        <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center">
          <span className="rounded-md bg-w-term-raise/90 px-2.5 py-1 text-w-xs text-w-term-dim">
            <Shimmer>Connecting to {deviceName(device)}</Shimmer>
          </span>
        </div>
      )}
      {phase === "ended" && (device || workshop) && (
        <div className="absolute inset-x-3 bottom-3 z-20 flex flex-wrap justify-center gap-1.5">
          <button type="button" onClick={() => onOpen(id, device)} className={NOTE_BTN}>
            <ArrowClockwise size={14} weight="bold" />
            {workshop ? "New shell" : `New shell on ${deviceName(device)}`}
          </button>
          {!workshop &&
            others.map((d) => {
              const full = blocked(d.id);
              return (
                <button key={d.id} type="button" aria-disabled={!!full} title={full ?? `Open a new shell on ${d.name} here`} onClick={() => !full && onOpen(id, d.id)} className={cn(NOTE_BTN, full && "opacity-40")}>
                  <Monitor size={14} weight="duotone" className="text-w-term-dim" />
                  {d.name}
                </button>
              );
            })}
        </div>
      )}
      {cover && <div className="absolute inset-0 z-20 flex items-center justify-center p-6 text-center">{cover}</div>}
      <div
        className={cn(
          "absolute right-2 top-1.5 z-20 flex items-center gap-0.5 rounded-md bg-w-term-bar/90 p-0.5 opacity-0 transition-opacity motion-reduce:transition-none",
          drag ? "pointer-events-none" : "focus-within:opacity-100 group-hover/pane:opacity-100",
        )}
      >
        {device && !workshop && <span className="max-w-[10rem] truncate px-1.5 text-w-2xs text-w-term-dim">{deviceName(device)}</span>}
        <span
          draggable
          role="button"
          tabIndex={-1}
          aria-label="Drag to move this terminal"
          title="Drag onto another terminal's edge to dock it there, or onto the tabs"
          onDragStart={(e) => {
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData("text/plain", deviceName(device));
            onDragPane(id);
          }}
          onDragEnd={onDragEnd}
          className={cn(PANE_BTN, "cursor-grab active:cursor-grabbing")}
        >
          <DotsSixVertical size={13} weight="bold" />
        </span>
        <button type="button" onClick={() => onSplit(id)} aria-label="Split right" title={(device && blocked(device)) || "Split right"} className={PANE_BTN}>
          <SquareSplitHorizontal size={13} weight="bold" />
        </button>
        <button type="button" onClick={() => onClosePane(id)} aria-label="Close this terminal" title="Close this terminal" className={cn(PANE_BTN, "hover:text-w-term-red")}>
          <X size={13} weight="bold" />
        </button>
      </div>
      {drag && !(drag.kind === "pane" && drag.id === id) && (
        <div
          className="absolute inset-0 z-30"
          onDragEnter={(e) => {
            acceptDrop(e);
            e.stopPropagation();
          }}
          onDragOver={(e) => {
            acceptDrop(e);
            e.stopPropagation();
            const rect = e.currentTarget.getBoundingClientRect();
            setZone(zoneAt(e.clientX - rect.left, e.clientY - rect.top, rect.width, rect.height));
          }}
          onDragLeave={() => setZone(null)}
          onDrop={(e) => {
            e.preventDefault();
            e.stopPropagation();
            const rect = e.currentTarget.getBoundingClientRect();
            const at = zoneAt(e.clientX - rect.left, e.clientY - rect.top, rect.width, rect.height);
            setZone(null);
            onDrop(id, at);
          }}
        >
          {zone && (
            <div
              aria-hidden
              className={cn(
                "pointer-events-none absolute rounded-md border border-w-term-focus/70 bg-w-term-focus/15",
                zone === "left" && "inset-y-1 left-1 w-[calc(50%-0.25rem)]",
                zone === "right" && "inset-y-1 right-1 w-[calc(50%-0.25rem)]",
                zone === "top" && "inset-x-1 top-1 h-[calc(50%-0.25rem)]",
                zone === "bottom" && "inset-x-1 bottom-1 h-[calc(50%-0.25rem)]",
                zone === "center" && "inset-1",
              )}
            />
          )}
        </div>
      )}
    </div>
  );
}

export function releaseEmbedTerminals(reportId: string, present: ReadonlySet<string>) {
  const storage = window.localStorage;
  const stored = Array.from({ length: storage.length }, (_, n) => storage.key(n)).filter((key): key is string => !!key);
  const gone = orphanKeys(stored, embedLayoutKey(`${reportId}:`), present);
  if (!gone.length) return;
  const natives = (keys: string[]) => keys.flatMap((key) => Object.values(loadLayout(storage, key)?.panes ?? {}).flatMap((pane) => (pane.native ? [pane.native] : [])));
  const kept = new Set(natives(stored.filter((key) => key.startsWith("world.terminals.") && !gone.includes(key))));
  for (const native of natives(gone)) if (!kept.has(native)) void wb.termClose(native).catch(() => {});
  for (const key of gone) saveLayout(storage, key, null);
}
