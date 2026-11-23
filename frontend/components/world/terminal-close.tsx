"use client";

import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, MotionConfig, motion } from "framer-motion";
import { ArrowLineDown, XSquare } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { closeChoiceKey, loadCloseChoice, saveCloseChoice, type CloseChoice, type ClosePlan } from "@/lib/terminal-close";
import { MODAL_MOTION, SCRIM_MOTION } from "./motion";
import { BTN_GHOST, CHUNKY, MODAL, PIXEL, SCRIM } from "./styles";

const choiceListeners = new Set<() => void>();

function watchChoice(listener: () => void): () => void {
  choiceListeners.add(listener);
  window.addEventListener("storage", listener);
  return () => {
    choiceListeners.delete(listener);
    window.removeEventListener("storage", listener);
  };
}

export function useCloseChoice(userId: string | null): [CloseChoice, (next: CloseChoice) => void] {
  const key = closeChoiceKey(userId);
  const choice = useSyncExternalStore(
    watchChoice,
    () => loadCloseChoice(window.localStorage, key),
    () => "ask" as const,
  );
  const setChoice = useCallback(
    (next: CloseChoice) => {
      saveCloseChoice(window.localStorage, key, next);
      for (const listener of choiceListeners) listener();
    },
    [key],
  );
  return [choice, setChoice];
}

export type ClosePick = { choice: Exclude<CloseChoice, "ask">; remember: boolean };

const CHOICE_BTN = cn(CHUNKY, "flex w-full items-start gap-3 px-3.5 py-3 text-left");

const FOCUSABLE = "button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex='-1'])";

function shown(el: Element | null): el is HTMLElement {
  return el instanceof HTMLElement && el.isConnected && el.getClientRects().length > 0;
}

function CloseDialog({ plan, onPick, onCancel }: { plan: ClosePlan; onPick: (pick: ClosePick) => void; onCancel: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  const minimize = useRef<HTMLButtonElement>(null);
  const [remember, setRemember] = useState(false);
  const rememberRef = useRef(remember);
  rememberRef.current = remember;
  const settled = useRef(false);
  const [closing, setClosing] = useState(false);
  const settle = () => {
    if (settled.current) return false;
    settled.current = true;
    setClosing(true);
    return true;
  };
  const pick = (next: ClosePick) => {
    if (settle()) onPick(next);
  };
  const cancel = () => {
    if (settle()) onCancel();
  };
  const handlers = useRef({ pick, cancel });
  handlers.current = { pick, cancel };
  const titleId = useId();
  const textId = useId();
  const one = plan.count === 1;

  useEffect(() => {
    const previous = document.activeElement;
    const root = box.current;
    minimize.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (!root || settled.current) return;
      e.stopImmediatePropagation();
      if (e.key === "Escape") {
        e.preventDefault();
        handlers.current.cancel();
        return;
      }
      const target = e.target instanceof HTMLElement ? e.target : null;
      if (e.key === "Enter" && !e.isComposing && !(target?.tagName === "BUTTON" && root.contains(target))) {
        e.preventDefault();
        handlers.current.pick({ choice: "minimize", remember: rememberRef.current });
        return;
      }
      if (e.key !== "Tab") return;
      const stops = Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(shown);
      if (!stops.length) return;
      const at = stops.indexOf(document.activeElement as HTMLElement);
      const next = e.shiftKey ? (at <= 0 ? stops.length - 1 : at - 1) : at === stops.length - 1 || at < 0 ? 0 : at + 1;
      e.preventDefault();
      stops[next].focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      const now = document.activeElement;
      const ours = !now || now === document.body || !!root?.contains(now);
      if (ours && shown(previous)) previous.focus();
    };
  }, []);

  return (
    <motion.div {...SCRIM_MOTION} className={cn(SCRIM, "z-[90]", closing && "pointer-events-none")} onPointerDown={(e) => e.target === e.currentTarget && cancel()}>
      <motion.div ref={box} {...MODAL_MOTION} role="dialog" aria-modal="true" aria-labelledby={titleId} aria-describedby={textId} className={cn(MODAL, "max-w-md p-5")}>
        <h2 id={titleId} className={cn(PIXEL, "text-balance text-w-xl leading-tight text-w-strong")}>
          Close the terminal?
        </h2>
        <p id={textId} className="mt-1.5 text-pretty text-w-sm leading-relaxed text-w-text2">
          {plan.where} {one ? "is" : "are"} still running.
        </p>
        <div className="mt-4 flex flex-col gap-2.5">
          <button ref={minimize} type="button" aria-keyshortcuts="Enter" onClick={() => pick({ choice: "minimize", remember })} className={cn(CHOICE_BTN, "bg-w-teal text-w-strong hover:bg-w-teal2")}>
            <ArrowLineDown size={18} weight="bold" className="mt-0.5 shrink-0" />
            <span className="flex min-w-0 flex-col gap-0.5">
              <span className="text-w-sm font-semibold">Minimize</span>
              <span className="text-w-xs">{one ? "Keep it running. It comes back just as it was." : "Keep them running. They come back just as they were."}</span>
            </span>
          </button>
          <button type="button" onClick={() => pick({ choice: "close_all", remember })} className={cn(CHOICE_BTN, "bg-w-card text-w-text hover:bg-w-rose")}>
            <XSquare size={18} weight="bold" className="mt-0.5 shrink-0 text-w-danger" />
            <span className="flex min-w-0 flex-col gap-0.5">
              <span className="text-w-sm font-semibold text-w-danger">Close all</span>
              <span className="text-w-xs text-w-text2">Ends {plan.where}.</span>
            </span>
          </button>
        </div>
        <div className="mt-4 flex items-center justify-between gap-3">
          <label className="flex min-w-0 cursor-pointer items-start gap-2 text-w-sm text-w-text">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} className="mt-0.5 h-4 w-4 shrink-0 accent-w-teal" />
            <span className="flex flex-col">
              <span>Don&apos;t ask again</span>
              <span className="text-w-xs text-w-muted">You can change it in Settings</span>
            </span>
          </label>
          <button type="button" onClick={cancel} className={BTN_GHOST}>
            Cancel
          </button>
        </div>
      </motion.div>
    </motion.div>
  );
}

export type CloseAsk = { id: number; plan: ClosePlan };

export function TerminalCloseDialog({ ask, onPick, onCancel }: { ask: CloseAsk | null; onPick: (pick: ClosePick) => void; onCancel: () => void }) {
  if (typeof document === "undefined") return null;
  return createPortal(
    <MotionConfig reducedMotion="user">
      <AnimatePresence>{ask && <CloseDialog key={ask.id} plan={ask.plan} onPick={onPick} onCancel={onCancel} />}</AnimatePresence>
    </MotionConfig>,
    document.body,
  );
}
