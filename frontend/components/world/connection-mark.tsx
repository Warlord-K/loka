"use client";

import { Checks, HourglassMedium, Minus, Pause, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";

export type Connection = "ok" | "connecting" | "off" | "paused" | "disabled";

const MARK = {
  ok: { Icon: Checks, tone: "text-w-ok", label: "Connected" },
  connecting: { Icon: HourglassMedium, tone: "text-w-warn", label: "Connecting" },
  off: { Icon: X, tone: "text-w-danger", label: "Not connected" },
  paused: { Icon: Pause, tone: "text-w-gold", label: "Paused" },
  disabled: { Icon: Minus, tone: "text-w-muted", label: "Off" },
} as const;

const DEVICE: Record<string, Connection> = {
  online: "ok",
  idle: "ok",
  running: "ok",
  connecting: "connecting",
  paused: "paused",
  unpaired: "off",
  offline: "off",
  error: "off",
};

export function deviceConnection(status: string | null | undefined): Connection {
  return (status && DEVICE[status]) || "off";
}

export function planeConnection(plane: "connecting" | "online" | "offline"): Connection {
  return plane === "online" ? "ok" : plane === "connecting" ? "connecting" : "off";
}

export function ConnectionMark({ state, size = 14, label, className }: { state: Connection; size?: number; label?: string; className?: string }) {
  const { Icon, tone, label: fallback } = MARK[state];
  return (
    <span role="img" aria-label={label ?? fallback} className={cn("inline-flex shrink-0 items-center", tone, className)}>
      <Icon size={size} weight="bold" />
    </span>
  );
}
