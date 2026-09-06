"use client";

import { useEffect, useRef } from "react";
import { X } from "@phosphor-icons/react";
import { motion } from "framer-motion";
import { MODAL_MOTION, SCRIM_MOTION } from "./motion";
import { cn } from "@/lib/utils";
import { MODAL, SCRIM, PIXEL } from "./styles";
import { Keys } from "./keycap";
import { CLOSE_BTN } from "./settings-shell";

export const SHORTCUTS: [string | string[], string][] = [
  [["⌘/Ctrl", "K"], "Search agents, messages and commands"],
  [["⌘/Ctrl", "1-9"], "Open a recent conversation"],
  [["⌘/Ctrl", "F"], "Find in the open window (Enter for next)"],
  [["Alt", "↑", "↓"], "Previous or next chat (add Shift for the next unread)"],
  ["↑", "Edit your last channel message (empty message box)"],
  [["⌘/Ctrl", "+", "-"], "Bigger or smaller text (⌘/Ctrl 0 resets)"],
  ["?", "Show these shortcuts"],
  [["W", "A", "S", "D"], "Walk (or the arrow keys, hold Shift to run)"],
  ["E", "Talk to whoever you face"],
  [["V"], "Hold to dictate to the open or nearest agent"],
  ["Q", "Quest board"],
  ["C", "Chat"],
  ["K", "Quick chat with an agent"],
  ["H", "Session history"],
  ["O", "Workshop: work with an agent in a folder"],
  ["L", "Lists"],
  ["R", "Reports"],
  ["M", "Map"],
  ["T", "Terminal"],
  ["N", "Notifications"],
  ["P", "Your main agent"],
  ["Y", "Your profile and settings"],
  ["Esc", "Close a window"],
];

export function ShortcutList({ className }: { className?: string }) {
  return (
    <dl className={cn("grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-w-sm", className)}>
      {SHORTCUTS.map(([k, v]) => (
        <div key={v} className="contents">
          <dt>
            <Keys keys={Array.isArray(k) ? k : [k]} />
          </dt>
          <dd className="text-w-text2">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function ShortcutSheet({ onClose }: { onClose: () => void }) {
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" && e.key !== "?") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      previous?.focus();
    };
  }, [onClose]);
  return (
    <motion.div {...SCRIM_MOTION} className={cn(SCRIM, "z-[80]")} onPointerDown={(e) => e.target === e.currentTarget && onClose()}>
      <motion.div
        {...MODAL_MOTION}
        role="dialog"
        aria-modal="true"
        aria-label="Keyboard shortcuts"
        className={cn(MODAL, "max-h-[85vh] max-w-md p-4 sm:p-5")}
      >
        <div className="mb-3 flex items-center justify-between gap-3">
          <h2 className={cn(PIXEL, "text-w-xl leading-none text-w-strong")}>Keyboard shortcuts</h2>
          <button ref={closeRef} type="button" onClick={onClose} aria-label="Close" className={CLOSE_BTN}>
            <X size={16} weight="bold" />
          </button>
        </div>
        <ShortcutList />
        <p className="mt-3 text-w-xs text-w-muted">Letter keys work when you are not typing in a field.</p>
      </motion.div>
    </motion.div>
  );
}
