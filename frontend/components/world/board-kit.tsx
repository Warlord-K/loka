"use client";

import { forwardRef } from "react";
import { cn } from "@/lib/utils";
import { COUNT, EASE } from "./styles";

type ColumnProps = {
  label: string;
  count: number;
  icon?: React.ReactNode;
  over?: boolean;
  compact?: boolean;
  empty?: React.ReactNode;
  className?: string;
  children?: React.ReactNode;
} & Omit<React.HTMLAttributes<HTMLDivElement>, "children" | "className">;

export const BoardColumn = forwardRef<HTMLDivElement, ColumnProps>(function BoardColumn(
  { label, count, icon, over = false, compact = false, empty, className, children, ...rest },
  ref,
) {
  return (
    <section
      ref={ref}
      aria-label={`${label}, ${count}`}
      {...rest}
      className={cn(
        "flex min-h-0 shrink-0 snap-start flex-col rounded-xl border-2 transition-colors",
        EASE,
        compact ? "w-[min(13rem,64vw)] @4xl:w-auto @4xl:min-w-[9rem] @4xl:flex-[0.6]" : "w-[min(18rem,80vw)] @4xl:w-auto @4xl:min-w-[12rem] @4xl:flex-1",
        over ? "border-w-teal bg-w-aqua/70" : "border-transparent bg-w-sand/45",
        className,
      )}
    >
      <header className="flex items-center gap-2 px-3 pb-2 pt-3">
        {icon}
        <h3 className="min-w-0 flex-1 truncate text-w-sm font-semibold text-w-strong">{label}</h3>
        <span className={COUNT}>{count}</span>
      </header>
      <ul className="min-h-0 flex-1 space-y-2 overflow-y-auto px-2 pb-2">
        {count === 0 && empty !== null && (
          <li className="rounded-lg border-2 border-dashed border-w-line px-3 py-5 text-center text-w-xs leading-relaxed text-w-faint">{empty ?? "Nothing here"}</li>
        )}
        {children}
      </ul>
    </section>
  );
});

export function BoardHint({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <p className={cn("hidden flex-wrap items-center gap-x-3 gap-y-1 border-t-2 border-dashed border-w-line2 px-4 py-2 text-w-2xs text-w-muted sm:flex sm:px-5", className)}>
      {children}
    </p>
  );
}
