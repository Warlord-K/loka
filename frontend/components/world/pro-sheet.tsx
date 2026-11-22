"use client";

import { useEffect } from "react";
import { ArrowSquareOut, Crown } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { WindowHeader } from "./settings-shell";
import { BTN_PRIMARY, MODAL } from "./styles";

export const PRO_WAITLIST_URL = "https://loka.ygpt.dev/#pro";

export const PRO_PITCH = "Work together with your team in organizations, use every device including iPhone, iPad and Apple Watch, and share agents and runners.";

export function ProSheet({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  return (
    <div role="dialog" aria-modal="true" aria-label="Loka Pro" className={cn(MODAL, "w-full max-w-md")}>
      <WindowHeader title="Loka Pro" subtitle="Coming soon" icon={Crown} onClose={onClose} />
      <div className="space-y-4 px-4 py-4 sm:px-5">
        <p className="text-w-base leading-relaxed text-w-text">
          Loka Pro, coming soon. {PRO_PITCH}
        </p>
        <a href={PRO_WAITLIST_URL} target="_blank" rel="noopener noreferrer" onClick={onClose} className={cn(BTN_PRIMARY, "w-full justify-center")}>
          Join the waitlist
          <ArrowSquareOut size={15} weight="bold" />
        </a>
      </div>
    </div>
  );
}
