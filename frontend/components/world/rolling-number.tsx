"use client";

import { useEffect, useState } from "react";
import { cn } from "@/lib/utils";

const DIGITS = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

export function RollingNumber({ value, className }: { value: string; className?: string }) {
  const [shown, setShown] = useState(() => value.replace(/\d/g, "0"));
  useEffect(() => {
    const frame = requestAnimationFrame(() => setShown(value));
    return () => cancelAnimationFrame(frame);
  }, [value]);
  const chars = [...shown];
  return (
    <span className={cn("relative inline-flex tabular-nums", className)}>
      <span className="sr-only">{value}</span>
      <span aria-hidden className="inline-flex">
        {chars.map((c, i) => {
          const key = chars.length - i;
          if (!/\d/.test(c))
            return (
              <span key={`s${key}`} className="inline-block">
                {c}
              </span>
            );
          return (
            <span key={`d${key}`} className="relative inline-block h-[1.15em] overflow-hidden leading-[1.15em]">
              <span className="invisible">0</span>
              <span
                className="absolute inset-x-0 top-0 flex flex-col transition-transform duration-700 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none"
                style={{ transform: `translateY(-${Number(c) * 1.15}em)` }}
              >
                {DIGITS.map((d) => (
                  <span key={d} className="block h-[1.15em] text-center">
                    {d}
                  </span>
                ))}
              </span>
            </span>
          );
        })}
      </span>
    </span>
  );
}
