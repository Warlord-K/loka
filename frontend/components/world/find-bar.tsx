"use client";

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "framer-motion";
import { CaretDown, CaretUp, MagnifyingGlass, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { takeEscape } from "@/lib/escape";
import { scrollWithin } from "@/lib/scroll";
import { isDesktopApp } from "@/lib/desktop";
import { POPOVER_MOTION } from "./motion";

const SCOPE = "[data-find-scope]";
const ALL = "loka-find";
const NOW = "loka-find-now";
const MAX = 2000;

type HighlightApi = { set: (name: string, h: unknown) => void; delete: (name: string) => void };

function registry(): HighlightApi | null {
  const css = (globalThis as { CSS?: { highlights?: HighlightApi } }).CSS;
  return css?.highlights ?? null;
}

function makeHighlight(ranges: Range[]): unknown {
  const Ctor = (globalThis as { Highlight?: new (...r: Range[]) => unknown }).Highlight;
  return Ctor ? new Ctor(...ranges) : null;
}

function frontScope(): HTMLElement | null {
  const scopes = [...document.querySelectorAll<HTMLElement>(SCOPE)].filter((el) => el.isConnected && el.getClientRects().length > 0);
  const active = document.activeElement;
  return scopes.find((s) => active && s.contains(active) && s.querySelector(SCOPE) === null) ?? scopes[scopes.length - 1] ?? null;
}

function visible(el: Element): boolean {
  const check = (el as Element & { checkVisibility?: (o?: object) => boolean }).checkVisibility;
  if (check) return check.call(el, { visibilityProperty: true, opacityProperty: false });
  return (el as HTMLElement).offsetParent !== null || getComputedStyle(el).position === "fixed";
}

function search(scope: HTMLElement, query: string, skip: Element | null): Range[] {
  const q = query.toLowerCase();
  if (!q) return [];
  const out: Range[] = [];
  const seen = new Map<Element, boolean>();
  const walker = document.createTreeWalker(scope, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent || !node.nodeValue || node.nodeValue.length < q.length) return NodeFilter.FILTER_REJECT;
      if (skip && skip.contains(parent)) return NodeFilter.FILTER_REJECT;
      if (parent.closest("script,style,noscript,textarea,input,[aria-hidden=true],[data-find-skip]")) return NodeFilter.FILTER_REJECT;
      let ok = seen.get(parent);
      if (ok === undefined) {
        ok = visible(parent);
        seen.set(parent, ok);
      }
      return ok ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
    },
  });
  for (let node = walker.nextNode(); node && out.length < MAX; node = walker.nextNode()) {
    const text = node.nodeValue!.toLowerCase();
    let at = text.indexOf(q);
    while (at >= 0 && out.length < MAX) {
      const r = document.createRange();
      r.setStart(node, at);
      r.setEnd(node, at + q.length);
      out.push(r);
      at = text.indexOf(q, at + q.length);
    }
  }
  return out;
}

function ownsFind(target: EventTarget | null): boolean {
  const el = target instanceof Element ? target : null;
  return Boolean(el?.closest(".cm-editor,.xterm,[data-own-find]"));
}

export function FindBar({ onNoScope }: { onNoScope?: () => void }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [count, setCount] = useState(0);
  const [index, setIndex] = useState(0);
  const [box, setBox] = useState<{ top: number; right: number } | null>(null);
  const scopeRef = useRef<HTMLElement | null>(null);
  const rangesRef = useRef<Range[]>([]);
  const inputRef = useRef<HTMLInputElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const indexRef = useRef(0);
  indexRef.current = index;
  const queryRef = useRef(query);
  queryRef.current = query;

  const paint = useCallback((at: number, scroll: boolean) => {
    const api = registry();
    const ranges = rangesRef.current;
    if (!api) return;
    if (!ranges.length) {
      api.delete(ALL);
      api.delete(NOW);
      return;
    }
    const current = ranges[Math.min(at, ranges.length - 1)];
    api.set(ALL, makeHighlight(ranges.filter((r) => r !== current)));
    api.set(NOW, makeHighlight([current]));
    if (scroll) {
      const el = current.startContainer.parentElement;
      const details = el?.closest("details:not([open])");
      if (details) (details as HTMLDetailsElement).open = true;
      scrollWithin(el, { block: "center" });
    }
  }, []);

  const run = useCallback(
    (keep: boolean, scroll: boolean) => {
      const scope = scopeRef.current;
      if (!scope || !scope.isConnected) {
        rangesRef.current = [];
        setCount(0);
        paint(0, false);
        return;
      }
      const prev = rangesRef.current[indexRef.current];
      const ranges = search(scope, queryRef.current.trim(), barRef.current);
      rangesRef.current = ranges;
      let at = 0;
      if (keep && prev) {
        const found = ranges.findIndex((r) => r.startContainer === prev.startContainer && r.startOffset === prev.startOffset);
        if (found >= 0) at = found;
        else {
          const later = ranges.findIndex((r) => prev.compareBoundaryPoints(Range.START_TO_START, r) <= 0);
          at = later >= 0 ? later : 0;
        }
      }
      setCount(ranges.length);
      setIndex(at);
      paint(at, scroll);
    },
    [paint],
  );

  const close = useCallback(() => {
    setOpen(false);
    rangesRef.current = [];
    registry()?.delete(ALL);
    registry()?.delete(NOW);
  }, []);

  const step = useCallback(
    (dir: 1 | -1) => {
      const n = rangesRef.current.length;
      if (!n) return;
      const next = (indexRef.current + dir + n) % n;
      setIndex(next);
      paint(next, true);
    },
    [paint],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === "g" && open) {
        e.preventDefault();
        step(e.shiftKey ? -1 : 1);
        return;
      }
      if (k !== "f" || e.shiftKey || ownsFind(e.target)) return;
      const scope = frontScope();
      if (!scope) {
        if (isDesktopApp() && onNoScope) {
          e.preventDefault();
          onNoScope();
        }
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      const field = scope.querySelector<HTMLInputElement>("input[data-find-input]");
      if (field && visible(field)) {
        if (open) close();
        field.focus();
        field.select();
        return;
      }
      scopeRef.current = scope;
      const picked = window.getSelection()?.toString().trim();
      if (picked && picked.length < 80 && !picked.includes("\n")) setQuery(picked);
      setOpen(true);
      window.requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.select();
      });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, step, close, onNoScope]);

  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      const scope = scopeRef.current;
      if (!scope?.isConnected) return close();
      const r = scope.getBoundingClientRect();
      const head = scope.querySelector("header")?.getBoundingClientRect();
      const top = head && head.bottom - r.top < 140 ? head.bottom + 8 : r.top + 8;
      setBox({ top: Math.max(8, top), right: Math.max(8, window.innerWidth - r.right + 16) });
    };
    place();
    window.addEventListener("resize", place);
    const ro = new ResizeObserver(place);
    if (scopeRef.current) ro.observe(scopeRef.current);
    return () => {
      window.removeEventListener("resize", place);
      ro.disconnect();
    };
  }, [open, close]);

  useEffect(() => {
    if (!open) return;
    const t = window.setTimeout(() => run(false, true), 90);
    return () => window.clearTimeout(t);
  }, [open, query, run]);

  useEffect(() => {
    if (!open || !scopeRef.current) return;
    let t = 0;
    const mo = new MutationObserver((records) => {
      if (records.every((r) => barRef.current?.contains(r.target))) return;
      window.clearTimeout(t);
      t = window.setTimeout(() => run(true, false), 200);
    });
    mo.observe(scopeRef.current, { subtree: true, childList: true, characterData: true });
    return () => {
      window.clearTimeout(t);
      mo.disconnect();
    };
  }, [open, run]);

  useEffect(() => {
    if (!open) return;
    const outside = (e: PointerEvent) => {
      if (barRef.current?.contains(e.target as Node)) return;
      if (scopeRef.current && !scopeRef.current.contains(e.target as Node)) close();
    };
    window.addEventListener("pointerdown", outside, true);
    return () => window.removeEventListener("pointerdown", outside, true);
  }, [open, close]);

  useEffect(() => () => close(), [close]);

  if (typeof document === "undefined") return null;
  const supported = registry() !== null;

  return createPortal(
    <AnimatePresence>
      {open && box && (
        <motion.div
          ref={barRef}
          {...POPOVER_MOTION}
          role="search"
          aria-label="Find in this window"
          style={{ top: box.top, right: box.right }}
          className="fixed z-[90] flex items-center gap-1 rounded-xl border-2 border-w-ink bg-w-paper p-1 pl-2.5 text-w-text shadow-[0_3px_0_rgb(var(--w-ink)),0_14px_32px_-14px_rgb(0_0_0/0.5)]"
        >
          <MagnifyingGlass size={14} weight="bold" className="shrink-0 text-w-muted" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                step(e.shiftKey ? -1 : 1);
              } else if (e.key === "Escape") {
                takeEscape(e);
                e.stopPropagation();
                close();
              }
            }}
            placeholder="Find in this window"
            aria-label="Find in this window"
            spellCheck={false}
            className="h-7 w-44 bg-transparent text-w-sm outline-none placeholder:text-w-faint sm:w-56"
          />
          <span className={cn("min-w-[3.25rem] text-right text-w-2xs tabular-nums", query.trim() && !count ? "text-w-danger" : "text-w-muted")} aria-live="polite">
            {!supported ? "" : query.trim() ? (count ? `${index + 1} of ${count >= MAX ? `${MAX}+` : count}` : "No match") : ""}
          </span>
          <span className="mx-0.5 h-4 w-px bg-w-line2" aria-hidden />
          <button type="button" onClick={() => step(-1)} disabled={!count} title="Previous (Shift Enter)" aria-label="Previous match" className="grid h-7 w-7 place-items-center rounded-md text-w-text2 transition-colors hover:bg-w-sand hover:text-w-strong disabled:opacity-40">
            <CaretUp size={13} weight="bold" />
          </button>
          <button type="button" onClick={() => step(1)} disabled={!count} title="Next (Enter)" aria-label="Next match" className="grid h-7 w-7 place-items-center rounded-md text-w-text2 transition-colors hover:bg-w-sand hover:text-w-strong disabled:opacity-40">
            <CaretDown size={13} weight="bold" />
          </button>
          <button type="button" onClick={close} aria-label="Close find" className="grid h-7 w-7 place-items-center rounded-md text-w-text2 transition-colors hover:bg-w-sand hover:text-w-strong">
            <X size={13} weight="bold" />
          </button>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
