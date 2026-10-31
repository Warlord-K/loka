import type { Transition } from "framer-motion";

export const EASE_SMOOTH = [0.22, 1, 0.36, 1] as const;

const OPEN: Transition = { duration: 0.25, ease: EASE_SMOOTH };
const CLOSE: Transition = { duration: 0.15, ease: EASE_SMOOTH };

export const MODAL_MOTION = {
  initial: { opacity: 0, scale: 0.96 },
  animate: { opacity: 1, scale: 1, transition: OPEN },
  exit: { opacity: 0, scale: 0.96, transition: CLOSE },
};

export function scrimMotion(blur = 2) {
  const off = { opacity: 0, backdropFilter: "blur(0px)" };
  const on = { opacity: 1, backdropFilter: `blur(${blur}px)` };
  return {
    initial: off,
    animate: { ...on, transition: OPEN },
    exit: { ...off, transition: CLOSE },
  };
}

export const SCRIM_MOTION = scrimMotion(2);

export const NOTICE_MOTION = {
  initial: { opacity: 0, y: -10, scale: 0.97 },
  animate: { opacity: 1, y: 0, scale: 1, transition: OPEN },
  exit: { opacity: 0, y: -8, scale: 0.98, transition: { duration: 0.2, ease: EASE_SMOOTH } },
};

export const SWAP_MOTION = {
  initial: { opacity: 0, y: 4 },
  animate: { opacity: 1, y: 0, transition: { duration: 0.2, ease: EASE_SMOOTH } },
  exit: { opacity: 0, y: -4, transition: { duration: 0.12, ease: EASE_SMOOTH } },
};

export const POPOVER_MOTION = {
  initial: { opacity: 0, scale: 0.97 },
  animate: { opacity: 1, scale: 1, transition: OPEN },
  exit: { opacity: 0, scale: 0.99, transition: CLOSE },
};
