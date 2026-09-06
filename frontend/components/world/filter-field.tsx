"use client";

import { MagnifyingGlass, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";

export function FilterField({
  value,
  onChange,
  placeholder,
  className,
  onEnter,
  onArrowDown,
  findTarget,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  className?: string;
  onEnter?: () => void;
  onArrowDown?: () => void;
  findTarget?: boolean;
}) {
  return (
    <label className={cn("flex h-10 items-center gap-2 rounded-lg border-2 border-w-line2 bg-w-card px-3 transition-colors focus-within:border-w-ink focus-within:ring-2 focus-within:ring-w-teal/30", className)}>
      <MagnifyingGlass size={15} weight="bold" className="shrink-0 text-w-muted" />
      <input
        type="text"
        data-find-input={findTarget || undefined}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape" && value) {
            e.preventDefault();
            e.stopPropagation();
            e.nativeEvent.stopImmediatePropagation();
            onChange("");
          } else if (e.key === "Enter" && onEnter && !e.nativeEvent.isComposing) {
            e.preventDefault();
            onEnter();
          } else if (e.key === "ArrowDown" && onArrowDown) {
            e.preventDefault();
            onArrowDown();
          }
        }}
        placeholder={placeholder}
        aria-label={placeholder}
        className="min-w-0 flex-1 bg-transparent text-w-sm text-w-text outline-none placeholder:text-w-faint"
      />
      {value && (
        <button type="button" onClick={() => onChange("")} aria-label="Clear filter" className="rounded-md p-0.5 text-w-muted hover:bg-w-sand hover:text-w-text">
          <X size={12} weight="bold" />
        </button>
      )}
    </label>
  );
}
