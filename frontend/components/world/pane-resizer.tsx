"use client";

import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { cn } from "@/lib/utils";

const PREFIX = "world.pane.";

function rootRem(): number {
  return parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
}

function stored(key: string): number | null {
  try {
    const raw = window.localStorage.getItem(PREFIX + key);
    const n = raw === null ? NaN : Number(raw);
    return Number.isFinite(n) ? n : null;
  } catch {
    return null;
  }
}

export function usePaneWidth(key: string, initial: number, min: number, max: number) {
  const clamp = useCallback((v: number) => Math.min(max, Math.max(min, v)), [min, max]);
  const [width, setWidth] = useState<number | null>(null);
  useEffect(() => {
    const saved = stored(key);
    if (saved !== null) setWidth(clamp(saved));
  }, [key, clamp]);
  const set = useCallback(
    (v: number | null) => {
      const next = v === null ? null : clamp(Math.round(v * 4) / 4);
      setWidth(next);
      try {
        if (next === null) window.localStorage.removeItem(PREFIX + key);
        else window.localStorage.setItem(PREFIX + key, String(next));
      } catch {}
    },
    [key, clamp],
  );
  const style = (width === null ? {} : { "--pane-w": `${width}rem` }) as CSSProperties;
  return { width: width ?? initial, custom: width !== null, set, min, max, style };
}

export function PaneResizer({
  pane,
  edge,
  label,
  className,
}: {
  pane: ReturnType<typeof usePaneWidth>;
  edge: "left" | "right";
  label: string;
  className?: string;
}) {
  const [dragging, setDragging] = useState(false);
  const paneRef = useRef(pane);
  paneRef.current = pane;

  const start = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const target = e.currentTarget;
    target.setPointerCapture(e.pointerId);
    const rem = rootRem();
    const from = e.clientX;
    const base = paneRef.current.width;
    setDragging(true);
    const body = document.body.style;
    const cursor = body.cursor;
    const select = body.userSelect;
    body.cursor = "col-resize";
    body.userSelect = "none";
    const move = (ev: PointerEvent) => {
      const delta = (ev.clientX - from) / rem;
      paneRef.current.set(edge === "right" ? base + delta : base - delta);
    };
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
      target.removeEventListener("pointercancel", up);
      body.cursor = cursor;
      body.userSelect = select;
      setDragging(false);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
    target.addEventListener("pointercancel", up);
  };

  const onKey = (e: React.KeyboardEvent) => {
    const grow = edge === "right" ? "ArrowRight" : "ArrowLeft";
    const shrink = edge === "right" ? "ArrowLeft" : "ArrowRight";
    const step = e.shiftKey ? 4 : 1;
    if (e.key === grow) pane.set(pane.width + step);
    else if (e.key === shrink) pane.set(pane.width - step);
    else if (e.key === "Home") pane.set(pane.min);
    else if (e.key === "End") pane.set(pane.max);
    else if (e.key === "Enter") pane.set(null);
    else return;
    e.preventDefault();
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={Math.round(pane.width * 16)}
      aria-valuemin={Math.round(pane.min * 16)}
      aria-valuemax={Math.round(pane.max * 16)}
      tabIndex={0}
      title="Drag to resize, double-click to reset"
      onPointerDown={start}
      onDoubleClick={() => pane.set(null)}
      onKeyDown={onKey}
      className={cn(
        "group relative z-10 w-0 shrink-0 cursor-col-resize touch-none select-none outline-none",
        "before:absolute before:inset-y-0 before:-left-1.5 before:w-3 before:content-['']",
        "after:pointer-events-none after:absolute after:inset-y-0 after:-left-px after:w-0.5 after:rounded-full after:bg-w-teal after:opacity-0 after:transition-opacity after:duration-150 after:content-['']",
        "hover:after:opacity-60 focus-visible:after:opacity-100",
        dragging && "after:opacity-100",
        className,
      )}
    />
  );
}
