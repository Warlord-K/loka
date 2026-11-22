"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Quotes, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { CHUNKY } from "./styles";

export type MessageQuote = { text: string; label?: string; source?: string; context?: string };

export const QUOTE_EVENT = "world:quote";
export const MAIN_TARGET = "main";

const TEXT_MAX = 2000;
const CONTEXT_MAX = 4000;
const BLOCKS = "p, li, td, th, pre, blockquote, h1, h2, h3, h4, h5, h6";
const pending = new Map<string, { quote: MessageQuote; until: number }>();
const PICKUP_MS = 10_000;

export function sendQuote(target: string, quote: MessageQuote, keepMs = PICKUP_MS) {
  pending.set(target, { quote, until: Date.now() + keepMs });
  window.dispatchEvent(new CustomEvent(QUOTE_EVENT, { detail: { target, quote } }));
}

export function useQuote(target: string | null): [MessageQuote | null, (quote: MessageQuote | null) => void] {
  const [quote, setQuote] = useState<MessageQuote | null>(null);
  useEffect(() => {
    const waiting = target ? pending.get(target) : undefined;
    setQuote(waiting && Date.now() < waiting.until ? waiting.quote : null);
    if (!target) return;
    const onQuote = (e: Event) => {
      const detail = (e as CustomEvent<{ target: string; quote: MessageQuote }>).detail;
      if (detail?.target === target) setQuote(detail.quote);
    };
    window.addEventListener(QUOTE_EVENT, onQuote);
    return () => window.removeEventListener(QUOTE_EVENT, onQuote);
  }, [target]);
  const set = useCallback(
    (next: MessageQuote | null) => {
      if (target) pending.delete(target);
      setQuote(next);
    },
    [target],
  );
  return [quote, set];
}

export function quoteProps(source: string, label: string, target?: string) {
  return { className: "select-text", "data-quote": source, "data-quote-label": label, ...(target ? { "data-quote-target": target } : {}) };
}

function squash(text: string) {
  return text.replace(/\n{3,}/g, "\n\n").trim();
}

function contextFor(root: HTMLElement, range: Range): string | undefined {
  const whole = squash(root.innerText ?? root.textContent ?? "");
  if (whole.length <= CONTEXT_MAX) return whole;
  const start = range.startContainer instanceof Element ? range.startContainer : range.startContainer.parentElement;
  const block = start?.closest(BLOCKS);
  if (!block || !root.contains(block)) return whole.slice(0, CONTEXT_MAX);
  let heading: string | null = null;
  const headings = Array.from(root.querySelectorAll("h1, h2, h3, h4, h5, h6"));
  for (const h of headings) {
    if (h.compareDocumentPosition(block) & Node.DOCUMENT_POSITION_FOLLOWING || h === block) heading = (h.textContent ?? "").trim();
  }
  const parts = [block.previousElementSibling, block, block.nextElementSibling]
    .filter((el): el is Element => !!el && root.contains(el))
    .map((el) => squash((el as HTMLElement).innerText ?? el.textContent ?? ""));
  const text = [heading && heading !== parts[1] ? `Section: ${heading}` : null, ...parts].filter(Boolean).join("\n\n");
  return text.slice(0, CONTEXT_MAX);
}

function editable(node: Node | null) {
  const el = node instanceof Element ? node : node?.parentElement;
  return !!el?.closest("input, textarea, [contenteditable='true'], .cm-editor, .xterm");
}

export function QuoteSelection() {
  const [spot, setSpot] = useState<{ x: number; y: number } | null>(null);
  const picked = useRef<{ root: HTMLElement; range: Range } | null>(null);

  const read = useCallback(() => {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount === 0) return null;
    const range = selection.getRangeAt(0);
    const text = selection.toString().trim();
    if (!text || editable(range.commonAncestorContainer)) return null;
    const node = range.commonAncestorContainer;
    const el = node instanceof Element ? node : node.parentElement;
    const root = el?.closest<HTMLElement>("[data-quote]");
    if (!root) return null;
    return { root, range, text };
  }, []);

  useEffect(() => {
    let timer = 0;
    const update = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const found = read();
        if (!found) {
          picked.current = null;
          setSpot(null);
          return;
        }
        picked.current = { root: found.root, range: found.range.cloneRange() };
        setSpot(spotAbove(found.range.getBoundingClientRect()));
      }, 120);
    };
    const hide = () => {
      picked.current = null;
      setSpot(null);
    };
    document.addEventListener("selectionchange", update);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("selectionchange", update);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
    };
  }, [read]);

  const quote = () => {
    const found = picked.current;
    const text = window.getSelection()?.toString().trim() || found?.range.toString().trim();
    if (!found || !text) return;
    const { root, range } = found;
    const context = contextFor(root, range);
    sendQuote(root.dataset.quoteTarget || MAIN_TARGET, {
      text: text.slice(0, TEXT_MAX),
      label: root.dataset.quoteLabel || undefined,
      source: root.dataset.quote || undefined,
      context: context && context !== text ? context : undefined,
    });
    window.getSelection()?.removeAllRanges();
    picked.current = null;
    setSpot(null);
  };

  return <QuoteButton spot={spot} onQuote={quote} />;
}

export function QuoteButton({ spot, onQuote }: { spot: { x: number; y: number } | null; onQuote: () => void }) {
  if (!spot || typeof document === "undefined") return null;
  return createPortal(
    <div style={{ left: spot.x, top: spot.y }} className="fixed z-[1000] -translate-x-1/2 -translate-y-full">
      <button
        type="button"
        onPointerDown={(e) => e.preventDefault()}
        onClick={onQuote}
        className={cn(CHUNKY, "flex items-center gap-1.5 bg-w-card px-2.5 py-1 text-w-sm font-semibold text-w-strong motion-safe:animate-w-rise")}
        title="Quote this in your reply"
      >
        <Quotes size={14} weight="bold" />
        Quote
      </button>
    </div>,
    document.body,
  );
}

export function spotAbove(rect: { left: number; top: number; bottom: number; width: number }) {
  const x = Math.min(Math.max(rect.left + rect.width / 2, 48), window.innerWidth - 48);
  const y = rect.top > 56 ? rect.top - 8 : rect.bottom + 44;
  return { x, y };
}

export function QuotePreview({ quote, onClear }: { quote: MessageQuote; onClear: () => void }) {
  return (
    <div className="mx-2 mt-2 flex items-start gap-2 rounded-lg border-l-4 border-w-teal bg-w-sand/60 py-1.5 pl-2.5 pr-1.5">
      <Quotes size={14} weight="bold" className="mt-0.5 shrink-0 text-w-muted" />
      <div className="min-w-0 flex-1">
        {quote.label && <p className="truncate text-w-2xs font-semibold text-w-muted">{quote.label}</p>}
        <p className="line-clamp-2 whitespace-pre-wrap break-words text-w-sm text-w-text2">{quote.text}</p>
      </div>
      <button type="button" onClick={onClear} aria-label="Remove quote" title="Remove quote" className="grid h-6 w-6 shrink-0 place-items-center rounded-md text-w-muted hover:bg-w-paper hover:text-w-strong">
        <X size={12} weight="bold" />
      </button>
    </div>
  );
}

export function QuotedBlock({ quote, className }: { quote: MessageQuote; className?: string }) {
  return (
    <div className={cn("mb-1.5 border-l-4 border-w-line2 pl-2.5 text-w-sm text-w-muted", className)}>
      {quote.label && <p className="truncate text-w-2xs font-semibold">{quote.label}</p>}
      <p className="line-clamp-3 whitespace-pre-wrap break-words">{quote.text}</p>
    </div>
  );
}
