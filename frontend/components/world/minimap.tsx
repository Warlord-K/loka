"use client";

import { useEffect, useRef, useState, type RefObject } from "react";
import { ArrowsIn, ArrowsOut, MapTrifold, Signpost } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { LokaEngine, MinimapPalette } from "./engine";
import { HUD_ICON, HUD_SEG, PIXEL } from "./styles";

const SMALL = 132;
const LARGE = 300;
const SHOWN_KEY = "world.minimap";

export function readMinimapShown(): boolean {
  try {
    return localStorage.getItem(SHOWN_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveMinimapShown(shown: boolean): void {
  try {
    localStorage.setItem(SHOWN_KEY, shown ? "1" : "0");
  } catch {}
}

function token(css: CSSStyleDeclaration, name: string, alpha = 1): string {
  const v = css.getPropertyValue(name).trim();
  return v ? `rgb(${v} / ${alpha})` : "transparent";
}

function readPalette(): MinimapPalette {
  const css = getComputedStyle(document.documentElement);
  return {
    player: token(css, "--w-onscrim"),
    ring: token(css, "--w-onscrim", 0.55),
    edge: token(css, "--w-scrim", 0.85),
    place: token(css, "--w-alert"),
    exit: token(css, "--w-gold2"),
    npc: token(css, "--w-seafoam2"),
    house: token(css, "--w-paper", 0.7),
  };
}

export function MinimapChip({ shown, onToggle, grouped = false }: { shown: boolean; onToggle: () => void; grouped?: boolean }) {
  return (
    <button type="button" onClick={onToggle} aria-pressed={shown} aria-label={shown ? "Hide the map" : "Show the map"} title="Map (M)" className={cn(grouped ? HUD_SEG : HUD_ICON, shown && "bg-w-field")}>
      <MapTrifold size={20} weight={shown ? "fill" : "bold"} className={shown ? "text-w-teal" : undefined} />
    </button>
  );
}

export function Minimap({
  engine,
  area,
  open,
  onToggle,
  onTravel,
}: {
  engine: RefObject<LokaEngine | null>;
  area: string;
  open: boolean;
  onToggle: () => void;
  onTravel: () => void;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [aspect, setAspect] = useState(0.7);
  const width = open ? LARGE : SMALL;
  const height = Math.round(width * Math.min(1, Math.max(0.4, aspect)));

  useEffect(() => {
    let last = "";
    const draw = () => {
      const e = engine.current;
      if (!e || !canvas.current) return;
      const next = e.mapAspect();
      setAspect((a) => (Math.abs(a - next) > 0.01 ? next : a));
      const key = `${e.minimapKey()}|${document.documentElement.dataset.theme ?? ""}|${canvas.current.width}`;
      if (key === last) return;
      last = key;
      e.drawMinimap(canvas.current, readPalette());
    };
    draw();
    const id = window.setInterval(draw, 250);
    return () => window.clearInterval(id);
  }, [engine, open, area, height]);

  return (
    <div className="pointer-events-auto rounded-lg border-2 border-w-ink bg-w-paper/95 p-1 shadow-[0_3px_0_rgb(var(--w-ink))]">
      <div className="flex items-center gap-1 pb-1 pl-1">
        <span className={cn(PIXEL, "min-w-0 flex-1 truncate text-w-xs leading-none text-w-text")}>{area}</span>
        <button type="button" onClick={onTravel} aria-label="Travel across Loka" title="All of Loka" className="rounded-md p-0.5 text-w-faint transition hover:bg-w-field hover:text-w-text">
          <Signpost size={13} weight="bold" />
        </button>
        <button type="button" onClick={onToggle} aria-label={open ? "Shrink the map" : "Expand the map"} className="rounded-md p-0.5 text-w-faint transition hover:bg-w-field hover:text-w-text">
          {open ? <ArrowsIn size={13} weight="bold" /> : <ArrowsOut size={13} weight="bold" />}
        </button>
      </div>
      <button type="button" onClick={onToggle} aria-label={`Map of ${area}`} className="block overflow-hidden rounded-md">
        <canvas ref={canvas} width={width * 2} height={height * 2} className={cn("block h-auto [image-rendering:pixelated]", open ? "w-[300px]" : "w-[132px]")} />
      </button>
      {open && (
        <div className="flex w-[300px] items-center justify-between gap-2 px-1 pt-1">
          <p className="text-w-2xs text-w-muted">Gold marks roads to other areas, orange marks places.</p>
          <button type="button" onClick={onTravel} className={cn(PIXEL, "shrink-0 rounded-md border-2 border-w-ink bg-w-card px-2 py-0.5 text-w-xs transition hover:bg-w-sand")}>
            All of Loka
          </button>
        </div>
      )}
    </div>
  );
}
