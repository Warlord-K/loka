"use client";

import { cn } from "@/lib/utils";

export function Shimmer({ children, className, active = true, style }: { children: React.ReactNode; className?: string; active?: boolean; style?: React.CSSProperties }) {
  if (!active) return <span className={className}>{children}</span>;
  return (
    <span
      style={style}
      className={cn(
        "bg-[linear-gradient(100deg,rgb(var(--w-text2))_38%,rgb(var(--w-shimmer))_50%,rgb(var(--w-text2))_62%)] bg-[length:300%_100%] bg-clip-text text-transparent animate-text-shimmer motion-reduce:animate-none motion-reduce:bg-none motion-reduce:text-w-text2",
        className,
      )}
    >
      {children}
    </span>
  );
}
