"use client";

import { MagnifyingGlass } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { HUD_CHIP, HUD_ICON } from "./styles";
import { Keys, isMac } from "./keycap";

export function TopSearch({ onSearch }: { onSearch: () => void }) {
  const mac = isMac();
  return (
    <button
      type="button"
      onClick={onSearch}
      aria-label="Search everything"
      aria-keyshortcuts={mac ? "Meta+K" : "Control+K"}
      className={cn(
        HUD_CHIP,
        "pointer-events-auto absolute left-1/2 top-3 z-20 hidden h-11 w-64 -translate-x-1/2 items-center gap-2.5 pl-3.5 pr-2 text-left sm:top-4 xl:flex 2xl:w-[min(26vw,26rem)]",
      )}
    >
      <MagnifyingGlass size={17} weight="bold" className="shrink-0 text-w-muted" />
      <span className="min-w-0 flex-1 truncate text-w-sm text-w-muted">Search everything</span>
      <Keys keys={mac ? ["⌘", "K"] : ["Ctrl", "K"]} />
    </button>
  );
}

export function SearchChip({ onSearch }: { onSearch: () => void }) {
  return (
    <button type="button" onClick={onSearch} aria-label="Search everything" className={`${HUD_ICON} xl:hidden`}>
      <MagnifyingGlass size={19} weight="bold" className="text-w-text" />
    </button>
  );
}
