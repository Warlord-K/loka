"use client";

import { useEffect, useState, type CSSProperties } from "react";
import { motion } from "framer-motion";
import { cn } from "@/lib/utils";
import { Shimmer } from "./shimmer";
import { PIXEL } from "./styles";

const SHIMMER_MS = 2400;
const SHOW_AFTER_MS = 250;
let played = false;

export function WorldSplash({ exit = false }: { exit?: boolean }) {
  const [entering] = useState(() => !played);
  const [shimmer] = useState<CSSProperties | undefined>(() => (played ? { animationDelay: `-${Math.round(performance.now() % SHIMMER_MS)}ms` } : undefined));
  useEffect(() => {
    played = true;
  }, []);
  const rise = (step: number) =>
    entering
      ? { className: "motion-safe:animate-w-splash-in motion-reduce:animate-w-fade", style: { animationDelay: `${SHOW_AFTER_MS + step * 70}ms` } }
      : { className: "", style: undefined };

  return (
    <motion.div
      initial={false}
      exit={exit ? { opacity: 0, transition: { duration: 0.3, ease: [0.22, 1, 0.36, 1] } } : undefined}
      className="fixed inset-0 z-[70] grid place-items-center bg-w-paper text-w-text"
      role="status"
      aria-label="Loading Loka"
    >
      <div className="flex flex-col items-center">
        <div
          style={rise(0).style}
          className={cn("grid h-[4.5rem] w-[4.5rem] place-items-center rounded-2xl border-[3px] border-w-ink bg-w-card shadow-[0_5px_0_rgb(var(--w-ink))]", rise(0).className)}
        >
          <img src="/icon.svg" alt="" width={44} height={44} className="h-11 w-11" />
        </div>
        <p style={rise(1).style} className={cn(PIXEL, "mt-5 text-3xl leading-none tracking-wide text-w-strong", rise(1).className)}>
          Loka
        </p>
        <span style={rise(2).style} className={cn("mt-3 block text-w-sm", rise(2).className)}>
          <Shimmer style={shimmer}>Waking up the town</Shimmer>
        </span>
      </div>
    </motion.div>
  );
}
