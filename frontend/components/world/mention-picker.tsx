"use client";

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { cn } from "@/lib/utils";

export interface MentionCandidate {
  key: string;
  handle: string;
  label: string;
  hint: string;
  icon: ReactNode;
  member?: boolean;
}

export function mentionHandle(name: string): string {
  return name.replace(/[^a-zA-Z0-9]/g, "").toLowerCase();
}

const LIMIT = 8;

function activeQuery(value: string, caret: number): { start: number; query: string } | null {
  const before = value.slice(0, caret);
  const match = /(^|\s)@([a-zA-Z0-9]*)$/.exec(before);
  if (!match) return null;
  return { start: caret - match[2].length - 1, query: match[2].toLowerCase() };
}

export function useMentionPicker({
  value,
  onChange,
  inputRef,
  candidates,
}: {
  value: string;
  onChange: (value: string) => void;
  inputRef: RefObject<HTMLTextAreaElement | HTMLInputElement | null>;
  candidates: MentionCandidate[];
}) {
  const [moved, setMoved] = useState(0);
  const [index, setIndex] = useState(0);
  const [dismissed, setDismissed] = useState<number | null>(null);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    let frame = 0;
    const later = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => setMoved((n) => n + 1));
    };
    el.addEventListener("click", later);
    el.addEventListener("keyup", later);
    el.addEventListener("focus", later);
    return () => {
      cancelAnimationFrame(frame);
      el.removeEventListener("click", later);
      el.removeEventListener("keyup", later);
      el.removeEventListener("focus", later);
    };
  }, [inputRef]);

  const placeCaret = useRef<number | null>(null);
  useLayoutEffect(() => {
    const at = placeCaret.current;
    const node = inputRef.current;
    if (at === null || !node || node.value !== value) return;
    placeCaret.current = null;
    node.focus();
    node.setSelectionRange(at, at);
  }, [value, inputRef]);

  const el = inputRef.current;
  const caret = el && el.value === value ? (el.selectionStart ?? value.length) : value.length;
  const active = useMemo(() => activeQuery(value, Math.min(caret, value.length)), [value, caret, moved]);
  const matches = useMemo(() => {
    if (!active) return [];
    const q = active.query;
    const scored = candidates
      .map((c) => {
        const label = c.label.toLowerCase();
        const score = !q
          ? 1
          : c.handle.startsWith(q)
            ? 3
            : label.split(/\s+/).some((w) => w.startsWith(q))
              ? 2
              : c.handle.includes(q) || label.includes(q)
                ? 1
                : 0;
        return { c, score: score + (c.member ? 0.5 : 0) };
      })
      .filter((s) => s.score > 0.5 || (!q && s.score > 0));
    scored.sort((a, b) => b.score - a.score || a.c.label.localeCompare(b.c.label));
    return scored.slice(0, LIMIT).map((s) => s.c);
  }, [active, candidates]);

  const open = !!active && matches.length > 0 && dismissed !== active.start;

  useEffect(() => setIndex(0), [active?.query, active?.start]);

  const pick = useCallback(
    (candidate: MentionCandidate) => {
      if (!active) return;
      const before = value.slice(0, active.start);
      const after = value.slice(active.start + 1 + active.query.length).replace(/^\s*/, "");
      const inserted = `${before}@${candidate.handle} `;
      placeCaret.current = inserted.length;
      onChange(`${inserted}${after}`);
      setMoved((n) => n + 1);
    },
    [active, value, onChange],
  );

  const onKey = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>): boolean => {
      if (!open) return false;
      if (e.key === "ArrowDown" || (e.key === "n" && e.ctrlKey)) {
        e.preventDefault();
        setIndex((i) => (i + 1) % matches.length);
        return true;
      }
      if (e.key === "ArrowUp" || (e.key === "p" && e.ctrlKey)) {
        e.preventDefault();
        setIndex((i) => (i - 1 + matches.length) % matches.length);
        return true;
      }
      if ((e.key === "Tab" || e.key === "Enter") && !e.shiftKey && !e.nativeEvent.isComposing) {
        e.preventDefault();
        pick(matches[Math.min(index, matches.length - 1)]);
        return true;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (active) setDismissed(active.start);
        return true;
      }
      return false;
    },
    [open, matches, index, pick, active],
  );

  const popover = open ? (
    <div
      role="listbox"
      aria-label="Mention someone"
      className="absolute bottom-full left-0 z-30 mb-2 w-72 max-w-[calc(100vw-2rem)] origin-bottom-left animate-w-menu-in overflow-hidden rounded-xl border-2 border-w-ink bg-w-paper p-1.5 shadow-[0_4px_0_rgb(var(--w-ink)),0_16px_36px_-14px_rgb(0_0_0/0.45)] motion-reduce:animate-none"
    >
      <p className="px-3 pb-1 pt-1.5 text-w-2xs font-semibold text-w-muted">
        Tab or Enter to mention
      </p>
      {matches.map((c, i) => (
        <button
          key={c.key}
          type="button"
          role="option"
          aria-selected={i === index}
          onMouseDown={(e) => e.preventDefault()}
          onMouseEnter={() => setIndex(i)}
          onClick={() => pick(c)}
          className={cn(
            "flex w-full items-center gap-2 px-3 py-1.5 text-left text-w-sm",
            i === index ? "bg-w-teal text-w-strong" : "hover:bg-w-sand",
          )}
        >
          <span className="shrink-0">{c.icon}</span>
          <span className="min-w-0 flex-1 truncate font-medium">{c.label}</span>
          <span className={cn("shrink-0 truncate text-w-2xs", i === index ? "text-w-strong/80" : "text-w-muted")}>
            @{c.handle}
            {c.hint ? ` · ${c.hint}` : ""}
          </span>
        </button>
      ))}
    </div>
  ) : null;

  return { popover, onKey };
}
