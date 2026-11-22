"use client";

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowSquareOut, CaretDown, CaretLeft, CaretRight, DownloadSimple, FilmStrip, X } from "@phosphor-icons/react";
import { isDesktopApp } from "@/lib/desktop";
import { cn } from "@/lib/utils";
import type { Attachment } from "@/lib/world";
import { MODAL_MOTION, SCRIM_MOTION } from "./motion";
import { ICON_BTN } from "./styles";
import { ZoomableImage } from "./zoomable-image";

const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const VIDEO_TYPES = ["video/mp4", "video/webm", "video/quicktime"];

export const isImage = (mime: string) => IMAGE_TYPES.includes(mime);
export const isVideo = (mime: string) => VIDEO_TYPES.includes(mime);
export const isMedia = (mime: string) => isImage(mime) || isVideo(mime);

export const mediaUrl = (a: Attachment) => a.url || `/api/world/uploads/${a.id}`;

export function mediaSummary(items: Attachment[]): string {
  const videos = items.filter((a) => isVideo(a.mime)).length;
  const pictures = items.length - videos;
  const part = (n: number, one: string, many: string) => (n === 0 ? null : n === 1 ? `1 ${one}` : `${n} ${many}`);
  return [part(pictures, "picture", "pictures"), part(videos, "video", "videos")].filter(Boolean).join(" and ");
}

const typesIntoField = (target: EventTarget | null) => target instanceof HTMLElement && !!target.closest("input,textarea,select,[contenteditable]:not([contenteditable=false])");

export function Lightbox({ items, start = 0, onClose }: { items: Attachment[]; start?: number; onClose: () => void }) {
  const [at, setAt] = useState(Math.min(start, items.length - 1));
  const item = items[at] ?? items[0];
  const many = items.length > 1;
  const zoomKeys = useRef<((e: KeyboardEvent) => boolean) | null>(null);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (typesIntoField(e.target) || document.querySelector("[cmdk-dialog]")) return;
      if (zoomKeys.current?.(e)) {
        e.preventDefault();
        e.stopImmediatePropagation();
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopImmediatePropagation();
        onClose();
      } else if (many && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
        e.preventDefault();
        e.stopImmediatePropagation();
        const step = e.key === "ArrowRight" ? 1 : -1;
        setAt((i) => (i + step + items.length) % items.length);
      } else if (!e.metaKey && !e.ctrlKey && !e.altKey) {
        if (e.key === "Backspace" || e.key === "Delete") e.preventDefault();
        e.stopImmediatePropagation();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose, many, items.length]);
  if (!item) return null;
  const url = mediaUrl(item);
  const desktop = isDesktopApp();
  return createPortal(
    <motion.div {...SCRIM_MOTION} className="fixed inset-0 z-[96] grid place-items-center bg-w-scrim/80 p-2 sm:p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label={item.name || "Media"}>
      <motion.div {...MODAL_MOTION} className="relative flex h-full w-full flex-col items-center gap-2" onClick={(e) => e.stopPropagation()}>
        <div className="flex w-full items-center justify-end gap-1">
          <span className="mr-auto min-w-0 truncate text-w-sm text-w-onscrim">
            {item.name}
            {many && <span className="ml-2 tabular-nums opacity-70">{at + 1} of {items.length}</span>}
          </span>
          {many && (
            <>
              <button type="button" onClick={() => setAt((i) => (i - 1 + items.length) % items.length)} className={ICON_BTN} title="Previous" aria-label="Previous">
                <CaretLeft size={18} weight="bold" />
              </button>
              <button type="button" onClick={() => setAt((i) => (i + 1) % items.length)} className={ICON_BTN} title="Next" aria-label="Next">
                <CaretRight size={18} weight="bold" />
              </button>
            </>
          )}
          {desktop ? (
            <a href={url} download={item.name || true} className={ICON_BTN} title="Save to Downloads" aria-label="Save to Downloads">
              <DownloadSimple size={18} weight="bold" />
            </a>
          ) : (
            <a href={url} target="_blank" rel="noopener noreferrer" className={ICON_BTN} title="Open in a new tab" aria-label="Open in a new tab">
              <ArrowSquareOut size={18} weight="bold" />
            </a>
          )}
          <button type="button" onClick={onClose} className={ICON_BTN} aria-label="Close">
            <X size={18} weight="bold" />
          </button>
        </div>
        {isVideo(item.mime) ? (
          <div className="grid min-h-0 w-full flex-1 place-items-center" onClick={(e) => e.target === e.currentTarget && onClose()}>
            <video key={url} src={url} controls autoPlay playsInline className="max-h-full max-w-full rounded-lg border-2 border-w-ink bg-black" />
          </div>
        ) : (
          <ZoomableImage key={url} src={url} alt={item.name || "Picture"} keys={zoomKeys} onDismiss={onClose} />
        )}
      </motion.div>
    </motion.div>,
    document.body,
  );
}

export function ZoomImage({ src, alt, className }: { src: string; alt: string; className?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)} title={alt || "Open larger"} className="inline-block cursor-zoom-in align-top">
        <img src={src} alt={alt} loading="lazy" className={className} />
      </button>
      <AnimatePresence>{open && <Lightbox key="zoom" items={[{ id: src, mime: "image/png", name: alt, url: src }]} onClose={() => setOpen(false)} />}</AnimatePresence>
    </>
  );
}

function Tile({ item, onOpen }: { item: Attachment; onOpen: () => void }) {
  const url = mediaUrl(item);
  if (isVideo(item.mime)) {
    return (
      <div className="relative">
        <video src={url} controls preload="metadata" playsInline className="max-h-64 max-w-[20rem] rounded-lg border-2 border-w-ink bg-black" />
        <button type="button" onClick={onOpen} className={cn(ICON_BTN, "absolute right-1 top-1 bg-w-paper/80")} title="Open larger" aria-label="Open larger">
          <ArrowSquareOut size={14} weight="bold" />
        </button>
      </div>
    );
  }
  return (
    <button type="button" onClick={onOpen} title={item.name || "Picture"} className="block cursor-zoom-in">
      <img src={url} alt={item.name || "Attached picture"} loading="lazy" className="max-h-56 max-w-[16rem] rounded-lg border-2 border-w-ink bg-w-field object-contain" />
    </button>
  );
}

export function MediaGrid({ items, collapsed }: { items: Attachment[]; collapsed?: boolean }) {
  const [open, setOpen] = useState(!collapsed);
  const [zoom, setZoom] = useState<number | null>(null);
  if (!items.length) return null;
  const thumbs = items.filter((a) => isImage(a.mime)).slice(0, 3);
  return (
    <>
      {collapsed && (
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="mt-1.5 flex items-center gap-2 rounded-lg border-2 border-w-line2 bg-w-field px-2 py-1 text-w-xs text-w-muted hover:text-w-text"
        >
          {!open && (
            <span className="flex -space-x-2">
              {thumbs.map((a) => (
                <img key={a.id} src={mediaUrl(a)} alt="" loading="lazy" className="size-6 rounded border border-w-ink bg-w-paper object-cover" />
              ))}
              {thumbs.length === 0 && <FilmStrip size={16} weight="duotone" />}
            </span>
          )}
          <span>{open ? "Hide" : `Show ${mediaSummary(items)}`}</span>
          <CaretDown size={12} weight="bold" className={cn("transition-transform motion-reduce:transition-none", open && "rotate-180")} />
        </button>
      )}
      {open && (
        <div className="mt-1.5 flex flex-wrap gap-2">
          {items.map((a, i) => (
            <Tile key={a.id} item={a} onOpen={() => setZoom(i)} />
          ))}
        </div>
      )}
      <AnimatePresence>{zoom !== null && <Lightbox key="zoom" items={items} start={zoom} onClose={() => setZoom(null)} />}</AnimatePresence>
    </>
  );
}
