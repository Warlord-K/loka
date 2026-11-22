"use client";

import { useCallback, useEffect, useRef, useState, type MutableRefObject, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { useReducedMotion } from "framer-motion";
import { ArrowsIn, MagnifyingGlassMinus, MagnifyingGlassPlus } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import {
  clampView,
  crisp,
  distance,
  fitView,
  isFiniteView,
  isZoomed,
  midpoint,
  panBy,
  toggleZoom,
  wheelFactor,
  zoomAt,
  zoomLimits,
  zoomTo,
  ZOOM_STEP,
  type Point,
  type Size,
  type ZoomView,
} from "@/lib/zoom";
import { ICON_BTN } from "./styles";

export type ZoomKeys = MutableRefObject<((e: KeyboardEvent) => boolean) | null>;

const ZOOM_BTN = cn(ICON_BTN, "disabled:opacity-40 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11");
const DOUBLE_TAP_MS = 300;
const TAP_SLOP = 6;

interface Gesture {
  view: ZoomView;
  from: Point;
  spread: number;
}

interface SafariGesture extends UIEvent {
  scale: number;
  clientX: number;
  clientY: number;
}

export function ZoomableImage({ src, alt, keys, onDismiss }: { src: string; alt: string; keys: ZoomKeys; onDismiss: () => void }) {
  const stageRef = useRef<HTMLDivElement>(null);
  const [stage, setStage] = useState<Size>({ w: 0, h: 0 });
  const [nat, setNat] = useState<Size | null>(null);
  const [view, setView] = useState<ZoomView>({ scale: 1, x: 0, y: 0 });
  const [glide, setGlide] = useState(false);
  const [grabbing, setGrabbing] = useState(false);
  const reduce = useReducedMotion();
  const live = useRef({ stage, nat, view });
  live.current = { stage, nat, view };
  const pointers = useRef(new Map<number, Point>());
  const gesture = useRef<Gesture | null>(null);
  const moved = useRef(false);
  const lastTap = useRef<{ at: number; point: Point } | null>(null);
  const lastPointer = useRef("mouse");
  const follow = useRef(true);
  const touches = useRef(new Set<number>());
  const multi = useRef(false);
  const [failed, setFailed] = useState(false);

  const local = useCallback((clientX: number, clientY: number): Point => {
    const rect = stageRef.current?.getBoundingClientRect();
    if (!rect) return { x: 0, y: 0 };
    return { x: clientX - rect.left - rect.width / 2, y: clientY - rect.top - rect.height / 2 };
  }, []);

  const apply = useCallback((next: (v: ZoomView, nat: Size, stage: Size) => ZoomView, animate: boolean) => {
    const { nat, stage } = live.current;
    if (!nat || !stage.w) return;
    setGlide(animate);
    setView((v) => {
      const result = next(v, nat, stage);
      if (!isFiniteView(result)) return v;
      follow.current = !isZoomed(result, nat, stage);
      return result;
    });
  }, []);

  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => setStage({ w: entry.contentRect.width, h: entry.contentRect.height }));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!nat || !stage.w) return;
    setGlide(false);
    setView((v) => (follow.current ? fitView(nat, stage) : clampView(v, nat, stage)));
  }, [nat, stage]);

  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const at = local(e.clientX, e.clientY);
      const factor = wheelFactor(e.deltaY, e.deltaMode, e.ctrlKey);
      apply((v, n, s) => zoomAt(v, v.scale * factor, at, n, s), false);
    };
    let pinchStart: ZoomView | null = null;
    const onGestureStart = (e: Event) => {
      e.preventDefault();
      pinchStart = touches.current.size > 0 ? null : live.current.view;
    };
    const onGestureChange = (e: Event) => {
      e.preventDefault();
      const g = e as SafariGesture;
      const start = pinchStart;
      if (!start || touches.current.size > 0 || !Number.isFinite(g.scale)) return;
      const at = Number.isFinite(g.clientX) && Number.isFinite(g.clientY) ? local(g.clientX, g.clientY) : { x: 0, y: 0 };
      apply((v, n, s) => zoomAt(v, start.scale * g.scale, at, n, s), false);
    };
    const onGestureEnd = (e: Event) => {
      e.preventDefault();
      pinchStart = null;
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    el.addEventListener("gesturestart", onGestureStart);
    el.addEventListener("gesturechange", onGestureChange);
    el.addEventListener("gestureend", onGestureEnd);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("gesturestart", onGestureStart);
      el.removeEventListener("gesturechange", onGestureChange);
      el.removeEventListener("gestureend", onGestureEnd);
    };
  }, [apply, local]);

  const center = { x: 0, y: 0 };
  const zoomIn = () => apply((v, n, s) => zoomAt(v, v.scale * ZOOM_STEP, center, n, s), true);
  const zoomOut = () => apply((v, n, s) => zoomAt(v, v.scale / ZOOM_STEP, center, n, s), true);
  const reset = () => apply((_, n, s) => fitView(n, s), true);

  useEffect(() => {
    keys.current = (e) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return false;
      const { nat, stage, view } = live.current;
      if (!nat || !stage.w) return false;
      if (e.key === "+" || e.key === "=") apply((v, n, s) => zoomAt(v, v.scale * ZOOM_STEP, { x: 0, y: 0 }, n, s), true);
      else if (e.key === "-" || e.key === "_") apply((v, n, s) => zoomAt(v, v.scale / ZOOM_STEP, { x: 0, y: 0 }, n, s), true);
      else if (e.key === "0") apply((_, n, s) => fitView(n, s), true);
      else if (e.key.startsWith("Arrow") && isZoomed(view, nat, stage)) {
        const step = Math.round(Math.min(stage.w, stage.h) * 0.15);
        const dx = e.key === "ArrowLeft" ? step : e.key === "ArrowRight" ? -step : 0;
        const dy = e.key === "ArrowUp" ? step : e.key === "ArrowDown" ? -step : 0;
        apply((v, n, s) => panBy(v, dx, dy, n, s), true);
      } else return false;
      return true;
    };
    return () => {
      keys.current = null;
    };
  }, [keys, apply]);

  const startGesture = () => {
    const points = [...pointers.current.values()];
    const { view } = live.current;
    if (points.length >= 2) gesture.current = { view, from: local(...xy(midpoint(points[0], points[1]))), spread: distance(points[0], points[1]) };
    else if (points.length === 1) gesture.current = { view, from: local(...xy(points[0])), spread: 0 };
    else gesture.current = null;
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    lastPointer.current = e.pointerType;
    e.currentTarget.setPointerCapture(e.pointerId);
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (e.pointerType === "touch") touches.current.add(e.pointerId);
    if (pointers.current.size === 1) {
      moved.current = false;
      multi.current = false;
    } else multi.current = true;
    startGesture();
    setGrabbing(true);
  };

  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const known = pointers.current.get(e.pointerId);
    const start = gesture.current;
    if (!known || !start) return;
    if (distance(known, { x: e.clientX, y: e.clientY }) > TAP_SLOP) moved.current = true;
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const points = [...pointers.current.values()];
    if (points.length >= 2 && start.spread > 0) {
      const to = local(...xy(midpoint(points[0], points[1])));
      const scale = start.view.scale * (distance(points[0], points[1]) / start.spread);
      apply((_, n, s) => zoomTo(start.view, scale, start.from, to, n, s), false);
    } else if (points.length === 1) {
      const to = local(e.clientX, e.clientY);
      apply((_, n, s) => panBy(start.view, to.x - start.from.x, to.y - start.from.y, n, s), false);
    }
  };

  const onPointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    touches.current.delete(e.pointerId);
    if (!pointers.current.delete(e.pointerId)) return;
    startGesture();
    if (pointers.current.size === 0) setGrabbing(false);
    if (multi.current) lastTap.current = null;
    if (e.pointerType !== "touch" || moved.current || multi.current || pointers.current.size > 0) return;
    const point = { x: e.clientX, y: e.clientY };
    const prev = lastTap.current;
    if (prev && e.timeStamp - prev.at < DOUBLE_TAP_MS && distance(prev.point, point) < TAP_SLOP * 4) {
      lastTap.current = null;
      const at = local(e.clientX, e.clientY);
      apply((v, n, s) => toggleZoom(v, at, n, s), true);
    } else lastTap.current = { at: e.timeStamp, point };
  };

  const onDoubleClick = (e: ReactMouseEvent<HTMLDivElement>) => {
    if (lastPointer.current === "touch") return;
    const at = local(e.clientX, e.clientY);
    apply((v, n, s) => toggleZoom(v, at, n, s), true);
  };

  const onClick = (e: ReactMouseEvent<HTMLDivElement>) => {
    if (moved.current) return;
    const { nat, stage, view } = live.current;
    if (!nat || !stage.w) return onDismiss();
    const at = local(e.clientX, e.clientY);
    const onImage = Math.abs(at.x - view.x) <= (nat.w * view.scale) / 2 && Math.abs(at.y - view.y) <= (nat.h * view.scale) / 2;
    if (!onImage && !isZoomed(view, nat, stage)) onDismiss();
  };

  const ready = !!nat && stage.w > 0;
  const zoomed = ready && isZoomed(view, nat, stage);
  const limits = ready ? zoomLimits(nat, stage) : { min: 1, max: 1 };
  const percent = Math.round(view.scale * 100);

  return (
    <>
      <div
        ref={stageRef}
        className={cn("relative min-h-0 w-full flex-1 touch-none select-none overflow-hidden", zoomed ? (grabbing ? "cursor-grabbing" : "cursor-grab") : "cursor-zoom-in")}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={onDoubleClick}
        onClick={onClick}
      >
        <img
          src={src}
          alt={alt}
          draggable={false}
          onLoad={(e) => {
            const img = e.currentTarget;
            setNat({ w: img.naturalWidth || 1, h: img.naturalHeight || 1 });
          }}
          onError={() => setFailed(true)}
          style={ready ? { width: nat.w * view.scale, height: nat.h * view.scale, transform: `translate(calc(-50% + ${view.x}px), calc(-50% + ${view.y}px))` } : undefined}
          className={cn(
            "pointer-events-none absolute left-1/2 top-1/2 max-w-none rounded-lg bg-w-field ring-2 ring-w-ink",
            ready && !failed ? "opacity-100" : "-translate-x-1/2 -translate-y-1/2 opacity-0",
            ready && crisp(view, nat) && "[image-rendering:pixelated]",
            glide && !reduce && "transition-[width,height,transform] duration-200 ease-[cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none",
          )}
        />
        {failed && (
          <p role="status" className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 rounded-lg border-2 border-w-ink bg-w-card px-4 py-3 text-center text-w-sm text-w-quiet">
            This picture could not be loaded.
          </p>
        )}
      </div>
      <div className="flex items-center justify-center gap-1">
        <button type="button" onClick={zoomOut} disabled={!ready || view.scale <= limits.min * 1.001} className={ZOOM_BTN} title="Zoom out (-)" aria-label="Zoom out" aria-keyshortcuts="-">
          <MagnifyingGlassMinus size={18} weight="bold" />
        </button>
        <output aria-live="polite" className="min-w-[3.75rem] select-none text-center text-w-sm font-medium tabular-nums text-w-onscrim">
          {ready ? `${percent}%` : ""}
        </output>
        <button type="button" onClick={zoomIn} disabled={!ready || view.scale >= limits.max * 0.999} className={ZOOM_BTN} title="Zoom in (+)" aria-label="Zoom in" aria-keyshortcuts="+">
          <MagnifyingGlassPlus size={18} weight="bold" />
        </button>
        <button type="button" onClick={reset} disabled={!zoomed} className={ZOOM_BTN} title="Fit to screen (0)" aria-label="Fit to screen" aria-keyshortcuts="0">
          <ArrowsIn size={18} weight="bold" />
        </button>
      </div>
    </>
  );
}

const xy = (p: Point): [number, number] => [p.x, p.y];
