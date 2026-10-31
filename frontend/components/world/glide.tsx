"use client";

import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

const WATCHED = ["data-selected", "data-highlighted", "aria-selected", "aria-current", "data-state"];

export default function Glide({ target, hover, className }: { target?: string; hover?: string; className?: string }) {
  const ref = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    const pill = ref.current;
    const box = pill?.parentElement;
    if (!pill || !box) return;
    let shown = false;
    let hovered: HTMLElement | null = null;
    let raf = 0;

    const place = () => {
      raf = 0;
      const el = hovered ?? (target ? box.querySelector<HTMLElement>(target) : null);
      if (!el || !el.isConnected || el.offsetParent === null) {
        pill.style.opacity = "0";
        shown = false;
        return;
      }
      const b = box.getBoundingClientRect();
      const r = el.getBoundingClientRect();
      const sx = box.offsetWidth ? b.width / box.offsetWidth : 1;
      const sy = box.offsetHeight ? b.height / box.offsetHeight : 1;
      const x = (r.left - b.left) / (sx || 1) + box.scrollLeft - box.clientLeft;
      const y = (r.top - b.top) / (sy || 1) + box.scrollTop - box.clientTop;
      if (!shown) pill.style.transition = "opacity 150ms ease";
      pill.style.transform = `translate3d(${x}px, ${y}px, 0)`;
      pill.style.width = `${r.width / (sx || 1)}px`;
      pill.style.height = `${r.height / (sy || 1)}px`;
      if (!shown) {
        void pill.offsetWidth;
        pill.style.transition = "";
      }
      pill.style.opacity = "1";
      shown = true;
    };
    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(place);
    };

    const observer = new MutationObserver(schedule);
    if (target) observer.observe(box, { subtree: true, childList: true, attributes: true, attributeFilter: WATCHED });

    const over = (e: PointerEvent) => {
      if (!hover || e.pointerType === "touch") return;
      const el = (e.target as Element | null)?.closest?.(hover) as HTMLElement | null;
      const next = el && box.contains(el) ? el : null;
      if (next !== hovered) {
        hovered = next;
        schedule();
      }
    };
    const leave = () => {
      if (!hovered) return;
      hovered = null;
      schedule();
    };
    if (hover) {
      box.addEventListener("pointerover", over);
      box.addEventListener("pointerleave", leave);
    }
    const resize = new ResizeObserver(schedule);
    resize.observe(box);
    schedule();

    return () => {
      cancelAnimationFrame(raf);
      observer.disconnect();
      resize.disconnect();
      box.removeEventListener("pointerover", over);
      box.removeEventListener("pointerleave", leave);
    };
  }, [target, hover]);

  return (
    <span
      ref={ref}
      aria-hidden
      className={cn(
        "pointer-events-none absolute left-0 top-0 z-0 opacity-0 transition-[transform,width,height,opacity] duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:!transition-[opacity]",
        className,
      )}
    />
  );
}
