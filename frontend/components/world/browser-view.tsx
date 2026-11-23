"use client";

import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowsIn, ArrowsOut, Cursor, Eye, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type WorldAgent } from "@/lib/world";
import { endedLine, idleLine, keyFrame, mouseButton, modifiers, pagePoint, pageToReopen, siteLabel, startView, viewKey, viewReducer, viewTitle, WATCH_BROWSER_EVENT, watchBrowser, withView, type BrowserHelpAsk, type WatchRequest } from "@/lib/browser-view";
import { MeaningIcon } from "@/lib/meaning-icons";
import { AgentFace } from "./channel-ui";
import { Shimmer } from "./shimmer";
import { POPOVER_MOTION } from "./motion";
import { BTN_GHOST, BTN_PRIMARY_SM, BTN_SECONDARY_SM, ICON_BTN, PILL_INFO, PILL_NEUTRAL } from "./styles";

const MOVE_EVERY_MS = 33;
const DOUBLE_CLICK_MS = 400;

export const HELP_DONE = "Done";
export const HELP_CANT = "I can't right now";

type Reset = { type: "reset" };

function reducer(state: ReturnType<typeof startView>, message: { type?: unknown; [key: string]: unknown } | Reset) {
  return message.type === "reset" ? startView() : viewReducer(state, message);
}

function BrowserView({ agent, questId, questTitle, help, expanded, onExpand, onClose }: { agent: WorldAgent; questId: string | null; questTitle: string | null; help: BrowserHelpAsk | null; expanded: boolean; onExpand: () => void; onClose: (interacted: boolean, answered: boolean) => void }) {
  const title = viewTitle(agent.name, { questId, questTitle });
  const [view, dispatch] = useReducer(reducer, undefined, startView);
  const [attempt, setAttempt] = useState(0);
  const [control, setControl] = useState(Boolean(help));
  const [answered, setAnswered] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const socket = useRef<WebSocket | null>(null);
  const stage = useRef<HTMLDivElement | null>(null);
  const keyboard = useRef<HTMLTextAreaElement | null>(null);
  const interacted = useRef(false);
  const [reopened, setReopened] = useState(false);
  const lastMove = useRef(0);
  const lastPress = useRef({ at: 0, x: 0, y: 0, clicks: 0 });
  const frameSize = useRef({ width: 0, height: 0 });
  frameSize.current = { width: view.frame?.width ?? 0, height: view.frame?.height ?? 0 };

  useEffect(() => {
    if (help) setControl(true);
  }, [help]);

  useEffect(() => {
    let gone = false;
    dispatch({ type: "reset" });
    setProblem(null);
    void worldApi
      .openBrowserView(agent.id, questId)
      .then((opened) => {
        if (gone) return;
        const ws = new WebSocket(opened.socket);
        socket.current = ws;
        ws.onopen = () => {
          if (!gone) dispatch({ type: "status", state: "connecting" });
        };
        ws.onmessage = (event) => {
          if (gone) return;
          try {
            dispatch(JSON.parse(String(event.data)));
          } catch {
            void 0;
          }
        };
        ws.onclose = () => {
          if (!gone) dispatch({ type: "closed", reason: "closed" });
        };
      })
      .catch((error: unknown) => {
        if (!gone) setProblem(error instanceof Error ? error.message : "Could not open this browser view.");
      });
    return () => {
      gone = true;
      const ws = socket.current;
      socket.current = null;
      if (ws) {
        try {
          if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "close" }));
        } catch {
          void 0;
        }
        ws.close();
      }
    };
  }, [agent.id, questId, attempt]);

  const send = useCallback((frame: Record<string, unknown>) => {
    const ws = socket.current;
    if (ws?.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify(frame));
    if (frame.type !== "open") interacted.current = true;
  }, []);

  const point = useCallback((e: { clientX: number; clientY: number }) => {
    const box = stage.current?.getBoundingClientRect();
    if (!box) return null;
    return pagePoint({ x: e.clientX, y: e.clientY }, box, frameSize.current);
  }, []);

  const reopen = answered ? null : pageToReopen(view, help, reopened);
  useEffect(() => {
    if (!reopen || socket.current?.readyState !== WebSocket.OPEN) return;
    setReopened(true);
    send({ type: "open", url: reopen });
  }, [reopen, send]);

  const live = view.phase === "live" || (view.phase === "idle" && view.frame !== null);
  const driving = control && view.phase === "live" && !answered;

  useEffect(() => {
    const el = stage.current;
    if (!el || !driving) return;
    const onWheel = (e: WheelEvent) => {
      const at = point(e);
      if (!at) return;
      e.preventDefault();
      send({ type: "mouse", event: "mouseWheel", x: at.x, y: at.y, dx: e.deltaX, dy: e.deltaY, modifiers: modifiers(e) });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [driving, point, send]);

  useEffect(() => {
    if (driving) keyboard.current?.focus({ preventScroll: true });
  }, [driving]);

  const press = (e: React.PointerEvent<HTMLDivElement>, event: "mousePressed" | "mouseReleased") => {
    if (!driving) return;
    const at = point(e);
    if (!at) return;
    e.preventDefault();
    if (event === "mousePressed") {
      keyboard.current?.focus({ preventScroll: true });
      e.currentTarget.setPointerCapture(e.pointerId);
      const last = lastPress.current;
      const again = e.timeStamp - last.at < DOUBLE_CLICK_MS && Math.abs(at.x - last.x) < 6 && Math.abs(at.y - last.y) < 6;
      lastPress.current = { at: e.timeStamp, x: at.x, y: at.y, clicks: again ? Math.min(last.clicks + 1, 3) : 1 };
    }
    send({ type: "mouse", event, x: at.x, y: at.y, button: mouseButton(e.button), clicks: lastPress.current.clicks, modifiers: modifiers(e) });
  };

  const move = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!driving || e.timeStamp - lastMove.current < MOVE_EVERY_MS) return;
    const at = point(e);
    if (!at) return;
    lastMove.current = e.timeStamp;
    send({ type: "mouse", event: "mouseMoved", x: at.x, y: at.y, button: e.buttons & 1 ? "left" : "none", modifiers: modifiers(e) });
  };

  const key = (e: React.KeyboardEvent<HTMLTextAreaElement>, kind: "down" | "up") => {
    if (!driving || e.nativeEvent.isComposing || e.key === "Process") return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "v") return;
    e.preventDefault();
    e.stopPropagation();
    send(keyFrame(kind, e));
  };

  const paste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (!driving) return;
    const text = e.clipboardData.getData("text/plain");
    if (!text) return;
    e.preventDefault();
    send({ type: "paste", text });
  };

  const typed = (e: { currentTarget: HTMLTextAreaElement; nativeEvent: Event }) => {
    const field = e.currentTarget;
    if ("isComposing" in e.nativeEvent && e.nativeEvent.isComposing) return;
    const text = field.value;
    field.value = "";
    if (driving && text) send({ type: "paste", text });
  };

  const answer = async (text: string) => {
    if (!help || sending) return;
    setSending(true);
    try {
      await worldApi.send(agent.id, text, [], undefined, help.cardId);
      setAnswered(text);
      setControl(false);
    } catch (error) {
      setProblem(error instanceof Error ? error.message : "That did not go through. Try again.");
    } finally {
      setSending(false);
    }
  };

  const site = help ? siteLabel(help.site) : "";
  const status =
    problem ??
    (view.phase === "starting" || view.phase === "connecting"
      ? "Connecting to the Mac"
      : view.phase === "waiting"
        ? "Waiting for the first picture"
        : view.phase === "idle"
          ? reopened && help
            ? `Opening ${siteLabel(help.site)}`
            : idleLine(agent.name, view.idle)
          : view.phase === "ended"
            ? endedLine(view.ended)
            : null);

  return (
    <motion.section
      {...POPOVER_MOTION}
      layout
      aria-label={title}
      data-find-skip=""
      className={cn(
        "pointer-events-auto flex flex-col overflow-hidden rounded-lg border-2 border-w-ink bg-w-paper text-w-text shadow-[0_4px_0_rgb(var(--w-ink))]",
        expanded ? "w-[min(64rem,calc(100vw-2rem))]" : "w-[26rem]",
      )}
    >
      <header className="flex items-center gap-2 border-b-2 border-w-line px-2.5 py-2">
        <AgentFace agent={agent} size={28} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-w-sm font-semibold text-w-strong" title={title}>
            {title}
          </p>
          <p className="truncate text-w-2xs text-w-muted" title={view.url ?? undefined}>
            {view.url ?? (view.phase === "live" ? "Live" : "Not browsing")}
          </p>
        </div>
        <button
          type="button"
          aria-pressed={control}
          disabled={view.phase !== "live" || Boolean(answered)}
          onClick={() => setControl((on) => !on)}
          className={cn(BTN_SECONDARY_SM, control && "bg-w-invert text-w-oninvert hover:bg-w-invert")}
          title={control ? "Back to watching only" : "Click and type in this browser"}
        >
          <Cursor size={14} weight="bold" />
          {control ? "In control" : "Take control"}
        </button>
        <button type="button" onClick={onExpand} aria-label={expanded ? "Make smaller" : "Make larger"} className={ICON_BTN}>
          {expanded ? <ArrowsIn size={14} weight="bold" /> : <ArrowsOut size={14} weight="bold" />}
        </button>
        <button type="button" onClick={() => onClose(interacted.current, answered !== null)} aria-label="Close" className={ICON_BTN}>
          <X size={14} weight="bold" />
        </button>
      </header>
      {help && (
        <div className="flex items-center gap-2 border-b-2 border-w-line bg-w-aqua/60 px-2.5 py-2 text-w-sm">
          <p className="min-w-0 flex-1 text-w-text">
            {answered ? (answered === HELP_DONE ? `Thanks. ${agent.name} carries on.` : `Told ${agent.name}.`) : `Do the ${site} part here, then press Done.`}
          </p>
          {!answered && (
            <>
              <button type="button" disabled={sending} onClick={() => void answer(HELP_CANT)} className={BTN_GHOST}>
                {HELP_CANT}
              </button>
              <button type="button" disabled={sending} onClick={() => void answer(HELP_DONE)} className={BTN_PRIMARY_SM}>
                {HELP_DONE}
              </button>
            </>
          )}
        </div>
      )}
      <div
        ref={stage}
        role="application"
        aria-label={driving ? `${agent.name}'s browser, you are in control` : `${agent.name}'s browser, watching only`}
        onPointerDown={(e) => press(e, "mousePressed")}
        onPointerUp={(e) => press(e, "mouseReleased")}
        onPointerMove={move}
        onContextMenu={(e) => driving && e.preventDefault()}
        className={cn(
          "relative aspect-video w-full select-none bg-w-sand outline-none",
          driving ? "cursor-default ring-2 ring-inset ring-w-teal" : "cursor-not-allowed",
        )}
      >
        <textarea
          ref={keyboard}
          aria-label={`Type into ${agent.name}'s browser`}
          tabIndex={driving ? 0 : -1}
          disabled={!driving}
          autoCapitalize="off"
          autoComplete="off"
          autoCorrect="off"
          spellCheck={false}
          onKeyDown={(e) => key(e, "down")}
          onKeyUp={(e) => key(e, "up")}
          onPaste={paste}
          onInput={typed}
          onCompositionEnd={typed}
          className="pointer-events-none absolute left-0 top-0 h-px w-px resize-none overflow-hidden border-0 p-0 opacity-0"
        />
        {view.frame && (
          <img
            src={view.frame.src}
            alt={`What ${agent.name} sees in the browser`}
            draggable={false}
            className={cn("pointer-events-none h-full w-full object-contain", view.phase !== "live" && "opacity-40")}
          />
        )}
        {status && (
          <div className="absolute inset-0 grid place-items-center p-6 text-center">
            <div className="flex max-w-[22rem] flex-col items-center gap-3">
              {view.phase === "starting" || view.phase === "connecting" || view.phase === "waiting" || (view.phase === "idle" && reopened && help) ? (
                <Shimmer className="text-w-sm">{status}</Shimmer>
              ) : (
                <p className="text-w-sm text-w-text2">{status}</p>
              )}
              {view.phase === "idle" && help?.url && (
                <button type="button" onClick={() => send({ type: "open", url: help.url })} className={BTN_SECONDARY_SM}>
                  {`Open ${site}`}
                </button>
              )}
              {(view.phase === "ended" || problem) && (
                <button type="button" onClick={() => setAttempt((n) => n + 1)} className={BTN_SECONDARY_SM}>
                  Watch again
                </button>
              )}
            </div>
          </div>
        )}
        {live && (
          <span className={cn("pointer-events-none absolute bottom-2 left-2", driving ? PILL_INFO : PILL_NEUTRAL)}>
            {driving ? <Cursor size={11} weight="bold" /> : <Eye size={11} weight="bold" />}
            {driving ? "You are in control" : "Watching"}
          </span>
        )}
        {view.note && <p className="absolute inset-x-2 top-2 rounded-md bg-w-rose px-2 py-1 text-w-xs text-w-danger">{view.note}</p>}
      </div>
    </motion.section>
  );
}

export function BrowserViews({ agents }: { agents: WorldAgent[] }) {
  const [views, setViews] = useState<WatchRequest[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);

  useEffect(() => {
    const onWatch = (event: Event) => {
      const request = (event as CustomEvent<WatchRequest>).detail;
      if (!request?.agentId) return;
      setViews((list) => withView(list, request));
    };
    window.addEventListener(WATCH_BROWSER_EVENT, onWatch);
    return () => window.removeEventListener(WATCH_BROWSER_EVENT, onWatch);
  }, []);

  const close = (request: WatchRequest, interacted: boolean, answered: boolean) => {
    if (request.help && interacted && !answered) void worldApi.send(request.agentId, HELP_DONE, [], undefined, request.help.cardId).catch(() => {});
    setViews((list) => list.filter((view) => viewKey(view) !== viewKey(request)));
    setExpanded((key) => (key === viewKey(request) ? null : key));
  };

  return (
    <div className="pointer-events-none fixed bottom-24 right-4 z-[55] flex max-w-[calc(100vw-2rem)] flex-row-reverse items-end gap-3">
      <AnimatePresence initial={false}>
        {[...views].reverse().map((request) => {
          const agent = agents.find((a) => a.id === request.agentId);
          if (!agent) return null;
          return (
            <BrowserView
              key={viewKey(request)}
              agent={agent}
              questId={request.questId ?? null}
              questTitle={request.questTitle ?? null}
              help={request.help ?? null}
              expanded={expanded === viewKey(request)}
              onExpand={() => setExpanded((key) => (key === viewKey(request) ? null : viewKey(request)))}
              onClose={(interacted, answered) => close(request, interacted, answered)}
            />
          );
        })}
      </AnimatePresence>
    </div>
  );
}

export function WatchBrowserButton({ agentId, questId, questTitle, className, iconOnly }: { agentId: string; questId?: string; questTitle?: string; className?: string; iconOnly?: boolean }) {
  const Icon = MeaningIcon.watchBrowser;
  return (
    <button
      type="button"
      onClick={() => watchBrowser({ agentId, questId, questTitle })}
      className={className ?? BTN_SECONDARY_SM}
      aria-label={iconOnly ? WATCH_LABEL : undefined}
      title="Watch this agent's browser live, and take over when it needs you"
    >
      <Icon size={iconOnly ? 18 : 14} weight="duotone" />
      {!iconOnly && WATCH_LABEL}
    </button>
  );
}

export const WATCH_LABEL = "Watch browser";

export function BrowserHelpButton({ agentId, cardId, help }: { agentId: string; cardId: string; help: { task_id?: string; title?: string; site: string; url: string } }) {
  const Icon = MeaningIcon.watchBrowser;
  return (
    <button
      type="button"
      onClick={() => watchBrowser({ agentId, questId: help.task_id, questTitle: help.title, help: { cardId, site: help.site, url: help.url } })}
      className={cn(BTN_PRIMARY_SM, "mt-2")}
    >
      <Icon size={14} weight="duotone" />
      {`Open the browser on ${siteLabel(help.site)}`}
    </button>
  );
}
