"use client";

import { useNow } from "@/lib/clock";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

export function relativeTime(iso: string, now: number = Date.now()): string {
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return "";
  const diff = at - now;
  const span = Math.abs(diff);
  const future = diff > 0;
  const say = (n: number, unit: string) => (future ? `in ${n}${unit}` : `${n}${unit} ago`);
  if (span < 45_000) return future ? "in a moment" : "just now";
  if (span < HOUR) return say(Math.max(1, Math.round(span / MINUTE)), "m");
  if (span < DAY) return say(Math.round(span / HOUR), "h");
  if (span < 2 * DAY && !future) return "yesterday";
  if (span < 7 * DAY) return say(Math.round(span / DAY), "d");
  const date = new Date(at);
  const sameYear = date.getFullYear() === new Date(now).getFullYear();
  return date.toLocaleDateString(undefined, sameYear ? { month: "short", day: "numeric" } : { month: "short", day: "numeric", year: "numeric" });
}

export function fullTime(iso: string): string {
  const at = Date.parse(iso);
  return Number.isNaN(at) ? "" : new Date(at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function RelativeTime({ at, className, prefix = "" }: { at: string; className?: string; prefix?: string }) {
  const now = useNow(30_000);
  return (
    <time dateTime={at} title={fullTime(at)} className={className}>
      {prefix}
      {relativeTime(at, now)}
    </time>
  );
}
