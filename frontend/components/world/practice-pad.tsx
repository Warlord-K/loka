"use client";

import { useEffect, useImperativeHandle, useRef, type Ref } from "react";
import { cn } from "@/lib/utils";
import {
  EMPTY_INK,
  inkDoc,
  inkReducer,
  PEN_SEEN_KEY,
  pointerChoice,
  pressureOf,
  rgbHex,
  smoothPressure,
  strokeWidth,
  toBoard,
  TOOL_ALPHA,
  type InkAction,
  type InkDoc,
  type InkHistory,
  type InkPoint,
  type InkStroke,
  type InkTool,
} from "@/lib/practice";

export type PadState = { count: number; canUndo: boolean; canRedo: boolean; version: number };

export type PadHandle = {
  act: (action: InkAction) => void;
  doc: () => InkDoc;
  png: () => Promise<Blob>;
};

type Board = { width: number; height: number };

type Pen = { tool: InkTool; color: string; size: number };

const MIN_STEP = 0.6;
const MAX_DPR = 3;

function tokenColor(token: string): string {
  const raw = getComputedStyle(document.documentElement).getPropertyValue(`--${token}`).trim();
  return raw ? `rgb(${raw.split(/\s+/).join(" ")})` : "rgb(0 0 0)";
}

let lightTokens: Map<string, string> | null = null;

function lightPalette(): Map<string, string> {
  if (lightTokens) return lightTokens;
  const found = new Map<string, string>();
  for (const sheet of Array.from(document.styleSheets)) {
    let rules: CSSRuleList;
    try {
      rules = sheet.cssRules;
    } catch {
      continue;
    }
    for (const rule of Array.from(rules)) {
      if (!(rule instanceof CSSStyleRule) || !rule.selectorText.includes(":root") || rule.selectorText.includes("dark")) continue;
      for (const name of Array.from(rule.style)) {
        if (name.startsWith("--w-") && !found.has(name.slice(2))) found.set(name.slice(2), rule.style.getPropertyValue(name).trim());
      }
    }
  }
  if (found.size) lightTokens = found;
  return found;
}

function lightChannels(token: string): string {
  return lightPalette().get(token) || getComputedStyle(document.documentElement).getPropertyValue(`--${token}`).trim();
}

function lightColor(token: string): string {
  const raw = lightChannels(token);
  return raw ? `rgb(${raw.split(/[\s,]+/).join(" ")})` : "rgb(0 0 0)";
}

function colorCache() {
  const seen = new Map<string, string>();
  return (token: string) => {
    let hit = seen.get(token);
    if (!hit) {
      hit = tokenColor(token);
      seen.set(token, hit);
    }
    return hit;
  };
}

const mid = (a: InkPoint, b: InkPoint) => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });

function dot(ctx: CanvasRenderingContext2D, stroke: InkStroke, at: InkPoint) {
  ctx.beginPath();
  ctx.arc(at.x, at.y, strokeWidth(stroke.tool, stroke.size, at.p) / 2, 0, Math.PI * 2);
  ctx.fill();
}

function segment(ctx: CanvasRenderingContext2D, stroke: InkStroke, a: InkPoint, b: InkPoint, c: InkPoint | null, first: boolean) {
  const from = first ? a : mid(a, b);
  const to = c ? mid(b, c) : b;
  ctx.lineWidth = widthStep(strokeWidth(stroke.tool, stroke.size, b.p));
  ctx.beginPath();
  ctx.moveTo(from.x, from.y);
  ctx.quadraticCurveTo(b.x, b.y, to.x, to.y);
  ctx.stroke();
}

function prime(ctx: CanvasRenderingContext2D, stroke: InkStroke, color: string) {
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.globalCompositeOperation = stroke.tool === "eraser" ? "destination-out" : "source-over";
}

const widthStep = (w: number) => Math.max(0.5, Math.round(w * 2) / 2);

function trace(ctx: CanvasRenderingContext2D, stroke: InkStroke) {
  const pts = stroke.points;
  if (pts.length === 0) return;
  dot(ctx, stroke, pts[0]);
  let width = -1;
  let open = false;
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1];
    const b = pts[i];
    const c = pts[i + 1] ?? null;
    const w = widthStep(strokeWidth(stroke.tool, stroke.size, b.p));
    const from = i === 1 ? a : mid(a, b);
    if (w !== width) {
      if (open) ctx.stroke();
      width = w;
      ctx.lineWidth = w;
      ctx.beginPath();
      ctx.moveTo(from.x, from.y);
      open = true;
    }
    const to = c ? mid(b, c) : b;
    ctx.quadraticCurveTo(b.x, b.y, to.x, to.y);
  }
  if (open) ctx.stroke();
}

function paint(target: CanvasRenderingContext2D, scratch: HTMLCanvasElement, strokes: InkStroke[], scale: number, color: (token: string) => string) {
  const layer = scratch.getContext("2d");
  if (!layer) return;
  for (const stroke of strokes) {
    const alpha = TOOL_ALPHA[stroke.tool];
    if (stroke.tool === "eraser" || alpha >= 1) {
      target.save();
      target.setTransform(scale, 0, 0, scale, 0, 0);
      prime(target, stroke, color(stroke.color));
      trace(target, stroke);
      target.restore();
      continue;
    }
    layer.setTransform(1, 0, 0, 1, 0, 0);
    layer.clearRect(0, 0, scratch.width, scratch.height);
    layer.setTransform(scale, 0, 0, scale, 0, 0);
    prime(layer, stroke, color(stroke.color));
    trace(layer, stroke);
    target.save();
    target.setTransform(1, 0, 0, 1, 0, 0);
    target.globalAlpha = alpha;
    target.drawImage(scratch, 0, 0);
    target.restore();
  }
}

function context(canvas: HTMLCanvasElement | null): CanvasRenderingContext2D | null {
  return canvas?.getContext("2d", { desynchronized: true }) ?? null;
}

export function PracticePad({
  ref,
  board,
  guide,
  pen,
  fingerDraw,
  label,
  onState,
  onPen,
  className,
}: {
  ref?: Ref<PadHandle>;
  board: Board;
  guide: boolean;
  pen: Pen;
  fingerDraw: boolean;
  label: string;
  onState: (state: PadState) => void;
  onPen?: () => void;
  className?: string;
}) {
  const frame = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLDivElement>(null);
  const baseRef = useRef<HTMLCanvasElement>(null);
  const liveRef = useRef<HTMLCanvasElement>(null);
  const scratch = useRef<HTMLCanvasElement | null>(null);
  const history = useRef<InkHistory>(EMPTY_INK);
  const scale = useRef(1);
  const colors = useRef(colorCache());
  const penRef = useRef(pen);
  penRef.current = pen;
  const fingerRef = useRef(fingerDraw);
  fingerRef.current = fingerDraw;
  const stateRef = useRef(onState);
  stateRef.current = onState;
  const onPenRef = useRef(onPen);
  onPenRef.current = onPen;
  const boardRef = useRef(board);
  boardRef.current = board;
  const drawing = useRef<{ id: number; type: string; stroke: InkStroke; drawn: number; pressure: number | null; pending: boolean } | null>(null);
  const version = useRef(0);
  const penSeen = useRef(false);
  const startedAt = useRef<number | null>(null);
  const raf = useRef<number | null>(null);

  const report = () => {
    const h = history.current;
    version.current += 1;
    stateRef.current({ count: h.strokes.length, canUndo: h.past.length > 0, canRedo: h.future.length > 0, version: version.current });
  };

  const redraw = () => {
    const base = baseRef.current;
    const ctx = context(base);
    if (!base || !ctx) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, base.width, base.height);
    if (!scratch.current) scratch.current = document.createElement("canvas");
    scratch.current.width = base.width;
    scratch.current.height = base.height;
    paint(ctx, scratch.current, history.current.strokes, scale.current, colors.current);
  };

  const fit = () => {
    const outer = frame.current;
    const inner = box.current;
    const base = baseRef.current;
    const live = liveRef.current;
    if (!outer || !inner || !base || !live) return;
    const b = boardRef.current;
    const ratio = b.width / b.height;
    const room = { w: outer.clientWidth, h: outer.clientHeight };
    if (room.w <= 0 || room.h <= 0) return;
    const w = Math.min(room.w, room.h * ratio);
    const h = w / ratio;
    inner.style.width = `${w}px`;
    inner.style.height = `${h}px`;
    const dpr = Math.min(window.devicePixelRatio || 1, MAX_DPR);
    const pw = Math.max(1, Math.round(w * dpr));
    const ph = Math.max(1, Math.round(h * dpr));
    scale.current = pw / b.width;
    for (const c of [base, live]) {
      if (c.width !== pw) c.width = pw;
      if (c.height !== ph) c.height = ph;
    }
    redraw();
  };

  useEffect(() => {
    fit();
    const outer = frame.current;
    if (!outer) return;
    const watch = new ResizeObserver(() => fit());
    watch.observe(outer);
    const theme = new MutationObserver(() => {
      colors.current = colorCache();
      redraw();
    });
    theme.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const media = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    const dprChanged = () => fit();
    media.addEventListener("change", dprChanged);
    return () => {
      watch.disconnect();
      theme.disconnect();
      media.removeEventListener("change", dprChanged);
      if (raf.current !== null) cancelAnimationFrame(raf.current);
    };
  }, [board.width, board.height]);

  const act = (action: InkAction) => {
    if (drawing.current) return;
    if (action.type === "reset") startedAt.current = null;
    history.current = inkReducer(history.current, action);
    redraw();
    report();
  };

  useImperativeHandle(ref, () => ({
    act,
    doc: () => {
      const doc = inkDoc(boardRef.current.width, boardRef.current.height, history.current.strokes);
      return { ...doc, strokes: doc.strokes.map((s) => ({ ...s, rgb: rgbHex(lightChannels(s.color)) ?? undefined })) };
    },
    png: () =>
      new Promise<Blob>((resolve, reject) => {
        const b = boardRef.current;
        const out = document.createElement("canvas");
        out.width = b.width;
        out.height = b.height;
        const ink = document.createElement("canvas");
        ink.width = b.width;
        ink.height = b.height;
        const layer = document.createElement("canvas");
        layer.width = b.width;
        layer.height = b.height;
        const octx = out.getContext("2d");
        const ictx = ink.getContext("2d");
        if (!octx || !ictx) return reject(new Error("Could not draw the picture."));
        const color = lightColor;
        octx.fillStyle = color("w-paper");
        octx.fillRect(0, 0, b.width, b.height);
        paint(ictx, layer, history.current.strokes, 1, color);
        octx.drawImage(ink, 0, 0);
        out.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("Could not export the picture."))), "image/png");
      }),
  }));

  const liveTarget = (stroke: InkStroke) => (stroke.tool === "eraser" ? context(baseRef.current) : context(liveRef.current));

  const drawPending = () => {
    raf.current = null;
    const d = drawing.current;
    if (!d) return;
    const ctx = liveTarget(d.stroke);
    if (!ctx) return;
    const pts = d.stroke.points;
    ctx.save();
    ctx.setTransform(scale.current, 0, 0, scale.current, 0, 0);
    prime(ctx, d.stroke, colors.current(d.stroke.color));
    if (d.drawn === 0 && pts.length > 0) {
      dot(ctx, d.stroke, pts[0]);
      d.drawn = 1;
    }
    for (let i = Math.max(1, d.drawn); i < pts.length - 1; i++) segment(ctx, d.stroke, pts[i - 1], pts[i], pts[i + 1], i === 1);
    d.drawn = Math.max(d.drawn, pts.length - 1);
    ctx.restore();
    d.pending = false;
  };

  const schedule = () => {
    const d = drawing.current;
    if (!d || d.pending) return;
    d.pending = true;
    raf.current = requestAnimationFrame(drawPending);
  };

  const push = (e: PointerEvent) => {
    const d = drawing.current;
    const canvas = liveRef.current;
    if (!d || !canvas) return;
    const rect = canvas.getBoundingClientRect();
    const samples = typeof e.getCoalescedEvents === "function" ? e.getCoalescedEvents() : [];
    for (const s of samples.length ? samples : [e]) {
      const at = toBoard(s.clientX, s.clientY, rect, boardRef.current);
      const last = d.stroke.points[d.stroke.points.length - 1];
      if (last && Math.hypot(at.x - last.x, at.y - last.y) < MIN_STEP) continue;
      d.pressure = smoothPressure(d.pressure, pressureOf(e.pointerType, s.pressure, s.buttons || e.buttons));
      d.stroke.points.push({ x: at.x, y: at.y, p: d.pressure, t: s.timeStamp - (startedAt.current ?? s.timeStamp) });
    }
  };

  const finish = (e: PointerEvent) => {
    const d = drawing.current;
    if (!d || d.id !== e.pointerId) return;
    if (raf.current !== null) {
      cancelAnimationFrame(raf.current);
      raf.current = null;
    }
    d.pending = false;
    drawPending();
    drawing.current = null;
    const live = liveRef.current;
    const lctx = context(live);
    if (live && lctx) {
      lctx.setTransform(1, 0, 0, 1, 0, 0);
      lctx.clearRect(0, 0, live.width, live.height);
    }
    history.current = inkReducer(history.current, { type: "add", stroke: d.stroke });
    const base = baseRef.current;
    const bctx = context(base);
    if (base && bctx && scratch.current) paint(bctx, scratch.current, [d.stroke], scale.current, colors.current);
    else redraw();
    report();
  };

  useEffect(() => {
    const canvas = liveRef.current;
    if (!canvas) return;
    try {
      if (window.localStorage.getItem(PEN_SEEN_KEY) === "1") {
        penSeen.current = true;
        onPenRef.current?.();
      }
    } catch {}
    const dropPalm = () => {
      const palm = drawing.current;
      if (!palm) return;
      if (raf.current !== null) {
        cancelAnimationFrame(raf.current);
        raf.current = null;
      }
      drawing.current = null;
      const live = liveRef.current;
      const lctx = context(live);
      if (live && lctx) {
        lctx.setTransform(1, 0, 0, 1, 0, 0);
        lctx.clearRect(0, 0, live.width, live.height);
      }
      if (palm.stroke.tool === "eraser") redraw();
      try {
        canvas.releasePointerCapture(palm.id);
      } catch {}
    };
    const down = (e: PointerEvent) => {
      if (e.pointerType === "pen" && !penSeen.current) {
        penSeen.current = true;
        try {
          window.localStorage.setItem(PEN_SEEN_KEY, "1");
        } catch {}
        onPenRef.current?.();
      }
      const choice = pointerChoice({
        pointerType: e.pointerType,
        penSeen: penSeen.current,
        fingerDraw: fingerRef.current,
        activeId: drawing.current?.id ?? null,
        activeType: drawing.current?.type ?? null,
        pointerId: e.pointerId,
      });
      if (choice === "ignore" || (e.pointerType === "mouse" && e.button !== 0)) return;
      if (choice === "takeover") dropPalm();
      e.preventDefault();
      try {
        canvas.setPointerCapture(e.pointerId);
      } catch {}
      if (startedAt.current === null) startedAt.current = e.timeStamp;
      const p = penRef.current;
      drawing.current = { id: e.pointerId, type: e.pointerType, stroke: { tool: p.tool, color: p.color, size: p.size, points: [] }, drawn: 0, pressure: null, pending: false };
      push(e);
      schedule();
    };
    const move = (e: PointerEvent) => {
      if (drawing.current?.id !== e.pointerId) return;
      e.preventDefault();
      push(e);
      schedule();
    };
    const up = (e: PointerEvent) => finish(e);
    canvas.addEventListener("pointerdown", down);
    canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerup", up);
    canvas.addEventListener("pointercancel", up);
    canvas.addEventListener("lostpointercapture", up);
    return () => {
      canvas.removeEventListener("pointerdown", down);
      canvas.removeEventListener("pointermove", move);
      canvas.removeEventListener("pointerup", up);
      canvas.removeEventListener("pointercancel", up);
      canvas.removeEventListener("lostpointercapture", up);
    };
  }, []);

  const liveAlpha = pen.tool === "eraser" ? 1 : TOOL_ALPHA[pen.tool];

  return (
    <div ref={frame} className={cn("grid min-h-0 min-w-0 place-items-center", className)}>
      <div ref={box} className="relative overflow-hidden rounded-xl border-2 border-w-ink bg-w-field shadow-[0_3px_0_rgb(var(--w-ink))]">
        {guide && (
          <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="pointer-events-none absolute inset-0 h-full w-full text-w-line2">
            <rect x="4" y="4" width="92" height="92" fill="none" stroke="currentColor" strokeWidth="0.5" vectorEffect="non-scaling-stroke" />
            <line x1="50" y1="4" x2="50" y2="96" stroke="currentColor" strokeWidth="1" strokeDasharray="6 6" vectorEffect="non-scaling-stroke" />
            <line x1="4" y1="50" x2="96" y2="50" stroke="currentColor" strokeWidth="1" strokeDasharray="6 6" vectorEffect="non-scaling-stroke" />
          </svg>
        )}
        <canvas ref={baseRef} aria-hidden className="absolute inset-0 h-full w-full" />
        <canvas
          ref={liveRef}
          role="img"
          aria-label={label}
          style={{ opacity: liveAlpha }}
          className={cn("absolute inset-0 h-full w-full touch-none select-none", pen.tool === "eraser" ? "cursor-cell" : "cursor-crosshair")}
        />
      </div>
    </div>
  );
}
