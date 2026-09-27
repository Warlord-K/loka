"use client";

import { useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { CHANGELOG, changelogSeen, markChangelogSeen } from "@/lib/changelog";
import { Header } from "./panels";
import { PANEL, PILL_WARN } from "./styles";
import { escapeTaken } from "@/lib/escape";

function Bold({ text }: { text: string }) {
  const parts = text.split("**");
  return (
    <>
      {parts.map((part, i) => (i % 2 === 1 ? <strong key={i} className="font-semibold text-w-text">{part}</strong> : <span key={i}>{part}</span>))}
    </>
  );
}

function longDate(date: string): string {
  const d = new Date(`${date}T12:00:00Z`);
  return d.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
}

export function ChangelogPanel({ onClose }: { onClose: () => void }) {
  const [seen] = useState(() => changelogSeen());
  useEffect(() => {
    markChangelogSeen();
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || escapeTaken(e)) return;
      e.preventDefault();
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div role="dialog" aria-modal="true" aria-label="What's new" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <Header title="What's new" subtitle="Newest first." onClose={onClose} />
      <div className="min-h-0 flex-1 overflow-y-auto p-4 sm:p-6">
        <ol className="mx-auto flex max-w-[70ch] flex-col gap-6">
          {CHANGELOG.map((entry) => {
            const fresh = seen !== null && entry.date > seen;
            return (
              <li key={`${entry.date}:${entry.title}`} className="rounded-lg border-2 border-w-line bg-w-card p-4 sm:p-5">
                <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                  <time dateTime={entry.date} className="text-w-xs font-semibold text-w-muted">
                    {longDate(entry.date)}
                  </time>
                  {fresh && <span className={PILL_WARN}>New</span>}
                </div>
                <h3 className="mt-1 text-w-lg font-semibold leading-tight text-w-strong">{entry.title}</h3>
                <ul className="mt-3 flex list-disc flex-col gap-1.5 pl-5 text-w-base leading-relaxed text-w-text2 marker:text-w-faint">
                  {entry.items.map((item, i) => (
                    <li key={i}>
                      <Bold text={item} />
                    </li>
                  ))}
                </ul>
              </li>
            );
          })}
        </ol>
      </div>
    </div>
  );
}
