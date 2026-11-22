export interface Size {
  w: number;
  h: number;
}

export interface Point {
  x: number;
  y: number;
}

export interface ZoomView {
  scale: number;
  x: number;
  y: number;
}

export const SMALL_IMAGE = 256;
export const MAX_ZOOM = 8;
export const ZOOM_STEP = 1.25;
const MAX_PIXEL_UPSCALE = 4;

export const isSmallImage = (nat: Size) => Math.max(nat.w, nat.h) <= SMALL_IMAGE;

export function fitScale(nat: Size, stage: Size): number {
  if (nat.w <= 0 || nat.h <= 0 || stage.w <= 0 || stage.h <= 0) return 1;
  const contain = Math.min(stage.w / nat.w, stage.h / nat.h);
  if (contain < 1) return contain;
  return isSmallImage(nat) ? Math.min(Math.floor(contain), MAX_PIXEL_UPSCALE) : 1;
}

export function zoomLimits(nat: Size, stage: Size): { min: number; max: number } {
  const min = fitScale(nat, stage);
  return { min, max: Math.max(MAX_ZOOM, min * MAX_PIXEL_UPSCALE) };
}

export const fitView = (nat: Size, stage: Size): ZoomView => ({ scale: fitScale(nat, stage), x: 0, y: 0 });

const clampAxis = (offset: number, image: number, stage: number) => {
  const room = Math.max(0, (image - stage) / 2);
  return Math.min(room, Math.max(-room, offset));
};

export const isFiniteView = (view: ZoomView) => Number.isFinite(view.scale) && Number.isFinite(view.x) && Number.isFinite(view.y) && view.scale > 0;

export function clampView(view: ZoomView, nat: Size, stage: Size, fallback: ZoomView = fitView(nat, stage)): ZoomView {
  if (!isFiniteView(view)) return fallback;
  const { min, max } = zoomLimits(nat, stage);
  const scale = Math.min(max, Math.max(min, view.scale));
  return { scale, x: clampAxis(view.x, nat.w * scale, stage.w), y: clampAxis(view.y, nat.h * scale, stage.h) };
}

export function zoomTo(view: ZoomView, scale: number, from: Point, to: Point, nat: Size, stage: Size): ZoomView {
  const { min, max } = zoomLimits(nat, stage);
  const next = Math.min(max, Math.max(min, scale));
  const ratio = next / view.scale;
  return clampView({ scale: next, x: to.x - (from.x - view.x) * ratio, y: to.y - (from.y - view.y) * ratio }, nat, stage, view);
}

export const zoomAt = (view: ZoomView, scale: number, at: Point, nat: Size, stage: Size) => zoomTo(view, scale, at, at, nat, stage);

export const panBy = (view: ZoomView, dx: number, dy: number, nat: Size, stage: Size) => clampView({ ...view, x: view.x + dx, y: view.y + dy }, nat, stage, view);

export const isZoomed = (view: ZoomView, nat: Size, stage: Size) => view.scale > fitScale(nat, stage) * 1.001;

export function toggleZoom(view: ZoomView, at: Point, nat: Size, stage: Size): ZoomView {
  if (isZoomed(view, nat, stage)) return fitView(nat, stage);
  const fit = fitScale(nat, stage);
  return zoomAt(view, Math.max(1, fit * 2), at, nat, stage);
}

export function wheelFactor(deltaY: number, deltaMode: number, pinch: boolean): number {
  const pixels = deltaMode === 1 ? deltaY * 16 : deltaMode === 2 ? deltaY * 400 : deltaY;
  const bounded = Math.max(-120, Math.min(120, pixels));
  return Math.exp(-bounded * (pinch ? 0.01 : 0.002));
}

export const crisp = (view: ZoomView, nat: Size) => isSmallImage(nat) || view.scale > 1;

export const distance = (a: Point, b: Point) => Math.hypot(a.x - b.x, a.y - b.y);

export const midpoint = (a: Point, b: Point): Point => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
