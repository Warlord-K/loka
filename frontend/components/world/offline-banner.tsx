"use client";

import { useEffect, useRef, useState } from "react";
import { AnimatePresence, MotionConfig, motion } from "framer-motion";
import { isDesktopApp } from "@/lib/desktop";
import { cn } from "@/lib/utils";
import { ConnectionMark } from "./connection-mark";
import { NOTICE_MOTION, SWAP_MOTION } from "./motion";
import { Shimmer } from "./shimmer";

const SHOW_AFTER_MS = 8_000;
const BACK_FOR_MS = 1_800;

type Phase = "hidden" | "down" | "back";

const CHIP =
  "pointer-events-auto flex h-11 max-w-full items-center gap-2.5 overflow-hidden rounded-lg border-2 border-w-ink bg-w-paper/95 pl-1.5 pr-3.5 text-left shadow-[0_3px_0_rgb(var(--w-ink)),0_14px_30px_-14px_rgb(0_0_0/0.5)] backdrop-blur-md";

export function OfflineBanner({ controlPlane }: { controlPlane: "connecting" | "online" | "offline" }) {
  const [phase, setPhase] = useState<Phase>("hidden");
  const shownRef = useRef(false);
  const down = controlPlane !== "online";

  useEffect(() => {
    if (down) {
      const timer = window.setTimeout(() => {
        shownRef.current = true;
        setPhase("down");
      }, SHOW_AFTER_MS);
      return () => window.clearTimeout(timer);
    }
    if (!shownRef.current) return;
    shownRef.current = false;
    setPhase("back");
    const timer = window.setTimeout(() => setPhase("hidden"), BACK_FOR_MS);
    return () => window.clearTimeout(timer);
  }, [down]);

  const back = phase === "back";
  const who = isDesktopApp() ? "Agents on this Mac keep working" : "Agents keep working";
  return (
    <MotionConfig reducedMotion="user">
      <AnimatePresence>
        {phase !== "hidden" && (
          <motion.div key="offline" {...NOTICE_MOTION} layout role="status" aria-live="polite" className={CHIP}>
            <motion.span
              layout="position"
              className={cn("grid h-7 w-7 shrink-0 place-items-center rounded-md transition-colors duration-200", back ? "bg-w-mint" : "bg-w-sand")}
            >
              <ConnectionMark state={back ? "ok" : controlPlane === "connecting" ? "connecting" : "off"} size={16} />
            </motion.span>
            <AnimatePresence mode="popLayout" initial={false}>
              {back ? (
                <motion.span key="back" {...SWAP_MOTION} className="whitespace-nowrap text-w-sm font-semibold text-w-strong">
                  Back online
                </motion.span>
              ) : (
                <motion.span key="down" {...SWAP_MOTION} className="flex min-w-0 items-center gap-2.5">
                  <span className="whitespace-nowrap text-w-sm font-semibold text-w-strong">Offline</span>
                  <span aria-hidden className="hidden h-4 w-0.5 shrink-0 rounded-sm bg-w-line2 sm:block" />
                  <span className="hidden truncate text-w-xs text-w-muted sm:block">{who}</span>
                  <span aria-hidden className="hidden text-w-xs text-w-faint sm:block">·</span>
                  <Shimmer className="shrink-0 whitespace-nowrap text-w-xs">Reconnecting</Shimmer>
                </motion.span>
              )}
            </AnimatePresence>
          </motion.div>
        )}
      </AnimatePresence>
    </MotionConfig>
  );
}
