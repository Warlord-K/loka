"use client";

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

type Tip = { text: string; x: number; y: number; side: "top" | "bottom"; open: boolean; glide: boolean };

const OPEN_DELAY = 500;
const WARM_WINDOW = 300;
const LEAVE_GRACE = 80;
const GAP = 8;
const EDGE = 8;

function holderOf(node: EventTarget | null): HTMLElement | null {
  if (!(node instanceof Element)) return null;
  const el = node.closest<HTMLElement>("[title], [data-tip]");
  if (!el || el.closest("[data-native-title]") || el instanceof HTMLIFrameElement) return null;
  return el;
}

const MAX_TIP = 64;

export function tipText(raw: string): string {
  const text = raw.replace(/\s+/g, " ").trim();
  if (text.length <= MAX_TIP) return text;
  const sentence = text.match(/^(.{8,}?[.:;!?])\s/);
  if (sentence && sentence[1].length <= MAX_TIP) return sentence[1].replace(/[.:;]$/, "");
  const cut = text.slice(0, MAX_TIP);
  const space = cut.lastIndexOf(" ");
  return `${(space > 24 ? cut.slice(0, space) : cut).replace(/[,.;:\s]+$/, "")}…`;
}

function adopt(el: HTMLElement): string | null {
  const raw = el.getAttribute("title");
  if (raw !== null) {
    el.removeAttribute("title");
    if (raw.trim()) {
      el.setAttribute("data-tip", raw);
      const named = el.getAttribute("aria-label") || el.getAttribute("aria-labelledby") || (el.textContent ?? "").trim();
      if (!named) el.setAttribute("aria-label", raw);
    } else el.removeAttribute("data-tip");
  }
  const tip = el.getAttribute("data-tip");
  return tip && tip.trim() ? tipText(tip) : null;
}

export function TooltipLayer() {
  const [tip, setTip] = useState<Tip | null>(null);
  const [mounted, setMounted] = useState(false);
  const bubble = useRef<HTMLDivElement>(null);
  const state = useRef({ target: null as HTMLElement | null, timer: 0, leave: 0, closedAt: 0, open: false, watch: null as MutationObserver | null });

  useEffect(() => {
    setMounted(true);
    const s = state.current;

    const place = (el: HTMLElement, text: string) => {
      const r = el.getBoundingClientRect();
      const below = r.top < 56;
      const glide = s.open;
      s.open = true;
      setTip({ text, x: r.left + r.width / 2, y: below ? r.bottom + GAP : r.top - GAP, side: below ? "bottom" : "top", open: true, glide });
    };

    const close = () => {
      window.clearTimeout(s.timer);
      window.clearTimeout(s.leave);
      s.watch?.disconnect();
      s.watch = null;
      if (s.open) s.closedAt = performance.now();
      s.open = false;
      s.target = null;
      setTip((t) => (t && t.open ? { ...t, open: false, glide: false } : t));
    };

    const show = (el: HTMLElement) => {
      const text = adopt(el);
      window.clearTimeout(s.leave);
      if (!text) return close();
      window.clearTimeout(s.timer);
      s.target = el;
      s.watch?.disconnect();
      s.watch = new MutationObserver(() => {
        if (s.target !== el) return;
        if (!el.isConnected) return close();
        const next = adopt(el);
        if (!next) return close();
        if (s.open) place(el, next);
      });
      s.watch.observe(el, { attributes: true, attributeFilter: ["title", "data-tip"] });
      const warm = s.open || performance.now() - s.closedAt < WARM_WINDOW;
      if (warm) place(el, text);
      else s.timer = window.setTimeout(() => s.target === el && el.isConnected && place(el, text), OPEN_DELAY);
    };

    const leaveSoon = () => {
      window.clearTimeout(s.leave);
      window.clearTimeout(s.timer);
      if (!s.open) {
        s.target = null;
        return;
      }
      s.leave = window.setTimeout(close, LEAVE_GRACE);
    };

    const onOver = (e: PointerEvent) => {
      if (e.pointerType === "touch") return;
      const el = holderOf(e.target);
      if (el && el === s.target) {
        window.clearTimeout(s.leave);
        return;
      }
      if (el) show(el);
      else if (s.target) leaveSoon();
    };
    const onOut = (e: PointerEvent) => {
      if (!s.target) return;
      const to = e.relatedTarget instanceof Node ? e.relatedTarget : null;
      if (to && s.target.contains(to)) return;
      if (!to) leaveSoon();
    };
    const onFocus = (e: FocusEvent) => {
      const el = holderOf(e.target);
      if (!el || !(e.target instanceof HTMLElement) || !e.target.matches(":focus-visible")) return;
      show(el);
    };
    const onBlur = () => s.target && close();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && s.target && close();
    const onDown = () => {
      if (!s.target) return;
      close();
      s.closedAt = 0;
    };

    document.addEventListener("pointerover", onOver, true);
    document.addEventListener("pointerout", onOut, true);
    document.addEventListener("focusin", onFocus, true);
    document.addEventListener("focusout", onBlur, true);
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("pointerdown", onDown, true);
    window.addEventListener("scroll", close, true);
    window.addEventListener("blur", close);
    return () => {
      close();
      document.removeEventListener("pointerover", onOver, true);
      document.removeEventListener("pointerout", onOut, true);
      document.removeEventListener("focusin", onFocus, true);
      document.removeEventListener("focusout", onBlur, true);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("blur", close);
    };
  }, []);

  useLayoutEffect(() => {
    const node = bubble.current;
    if (!tip || !node || !tip.open) return;
    const w = node.offsetWidth;
    const left = Math.min(Math.max(tip.x - w / 2, EDGE), window.innerWidth - w - EDGE);
    node.style.left = `${left}px`;
    node.style.top = `${tip.y}px`;
    node.style.transformOrigin = `${tip.x - left}px ${tip.side === "top" ? "100%" : "0%"}`;
  }, [tip]);

  if (!mounted) return null;
  const view = tip ?? { text: "", x: 0, y: 0, side: "top" as const, open: false, glide: false };
  return createPortal(
    <div
      ref={bubble}
      role="tooltip"
      aria-hidden={!view.open}
      data-open={view.open}
      data-glide={view.glide}
      data-side={view.side}
      className={[
        "pointer-events-none fixed z-[200] w-max max-w-[calc(100vw-16px)] overflow-hidden rounded-lg bg-w-invert px-2.5 py-1.5 text-w-xs font-medium leading-snug text-w-oninvert shadow-[0_8px_24px_-8px_rgb(0_0_0/0.5)] ring-1 ring-black/10",
        "whitespace-nowrap",
        "data-[side=top]:-translate-y-full",
        "opacity-0 [scale:0.96] transition-[opacity,scale] duration-150 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)]",
        "data-[open=true]:opacity-100 data-[open=true]:[scale:1]",
        "data-[open=false]:duration-100",
        "data-[glide=true]:transition-[opacity,scale,left,top] data-[glide=true]:duration-200",
        "motion-reduce:transition-opacity motion-reduce:[scale:1]",
      ].join(" ")}
    >
      <span key={view.text} className="block truncate animate-tip-text motion-reduce:animate-none">
        {view.text}
      </span>
    </div>,
    document.body,
  );
}
