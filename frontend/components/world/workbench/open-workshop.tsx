"use client";

import { useEffect, useRef, useState } from "react";
import { Hammer } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { requestWorkshop, workbenchSupport, type OpenWorkshopDetail } from "./bridge";

export function OpenWorkshop({
  detail,
  label,
  className,
}: {
  detail: OpenWorkshopDetail;
  label?: string;
  className?: string;
}) {
  const [note, setNote] = useState<"update" | null>(null);
  const box = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!note) return;
    const away = (event: PointerEvent) => {
      if (!box.current?.contains(event.target as Node)) setNote(null);
    };
    window.addEventListener("pointerdown", away);
    return () => window.removeEventListener("pointerdown", away);
  }, [note]);

  const open = async (event: React.MouseEvent) => {
    event.stopPropagation();
    const support = await workbenchSupport();
    if (support === "ready") requestWorkshop(detail);
    else setNote((current) => (current ? null : support));
  };

  return (
    <span ref={box} className="relative inline-flex">
      <button
        type="button"
        onClick={(event) => void open(event)}
        title="Open workshop"
        aria-label="Open workshop"
        className={cn(
          "inline-flex items-center gap-1 rounded-md border-2 border-w-ink bg-w-card px-1.5 py-0.5 text-w-2xs text-w-text shadow-[0_2px_0_rgb(var(--w-ink))] transition hover:bg-w-sand active:translate-y-[2px] active:shadow-none",
          className,
        )}
      >
        <Hammer size={12} weight="bold" />
        {label && <span>{label}</span>}
      </button>
      {note && (
        <span className="absolute right-0 top-full z-50 mt-1 w-56 rounded-lg border-2 border-w-ink bg-w-paper p-2 text-left text-w-xs text-w-text shadow-[0_3px_0_rgb(var(--w-ink))]">
          Update the World app to open the workshop.
        </span>
      )}
    </span>
  );
}
