"use client";

import { Fragment, type ReactNode } from "react";
import { FileText, GitPullRequest } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { Draft } from "@/lib/world";

const LINK = /\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)|<(https?:\/\/[^>|\s]+)(?:\|([^>\n]+))?>|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

export function LinkedText({ text, className }: { text: string; className?: string }) {
  const parts: ReactNode[] = [];
  let at = 0;
  for (const m of text.matchAll(LINK)) {
    const index = m.index ?? 0;
    if (index > at) parts.push(text.slice(at, index));
    const href = m[2] ?? m[3] ?? m[5];
    const label = m[1] ?? m[4] ?? m[5] ?? href;
    parts.push(
      <a
        key={index}
        href={href}
        target="_blank"
        rel="noopener noreferrer"
        onClick={(e) => e.stopPropagation()}
        className="break-all text-w-link underline decoration-w-teal/50 hover:decoration-w-link"
      >
        {label}
      </a>,
    );
    at = index + m[0].length;
  }
  if (at < text.length) parts.push(text.slice(at));
  return <p className={cn("whitespace-pre-wrap break-words", className)}>{parts}</p>;
}

type Row = { kind: "hunk" | "add" | "del" | "ctx"; text: string; old: number | null; new: number | null };

function parseDiff(diff: string, side: "LEFT" | "RIGHT", line: number | null): Row[] {
  const raw = diff.split("\n");
  const hasHeader = raw.some((l) => l.startsWith("@@"));
  const rows: Row[] = raw.map((l) => ({
    kind: l.startsWith("@@") ? "hunk" : l.startsWith("+") ? "add" : l.startsWith("-") ? "del" : "ctx",
    text: l.startsWith("@@") ? l : l.slice(1),
    old: null,
    new: null,
  }));
  if (hasHeader) {
    let oldNo = 0;
    let newNo = 0;
    for (const r of rows) {
      if (r.kind === "hunk") {
        const m = r.text.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)/);
        if (m) {
          oldNo = Number(m[1]);
          newNo = Number(m[2]);
        }
        continue;
      }
      if (r.kind !== "add") r.old = oldNo++;
      if (r.kind !== "del") r.new = newNo++;
    }
    return rows;
  }
  if (line) {
    const counted = rows.filter((r) => (side === "LEFT" ? r.kind !== "add" : r.kind !== "del")).length;
    let n = line - counted + 1;
    for (const r of rows) {
      if (side === "LEFT" ? r.kind !== "add" : r.kind !== "del") {
        if (side === "LEFT") r.old = n++;
        else r.new = n++;
      }
    }
  }
  return rows;
}

function prUrl(pr: string | null | undefined): string | null {
  const m = pr?.match(/^([\w.-]+\/[\w.-]+)#(\d+)$/);
  if (m) return `https://github.com/${m[1]}/pull/${m[2]}/files`;
  return pr && /^https:\/\/github\.com\//.test(pr) ? pr : null;
}

export function ReviewDiff({ draft }: { draft: Draft }) {
  const side = draft.side === "LEFT" ? "LEFT" : "RIGHT";
  const rows = draft.diff ? parseDiff(draft.diff, side, draft.line ?? null) : [];
  const first = draft.start_line ?? draft.line ?? null;
  const last = draft.line ?? null;
  const marked = (r: Row) => {
    const n = side === "LEFT" ? r.old : r.new;
    return n !== null && first !== null && last !== null && n >= first && n <= last;
  };
  const link = prUrl(draft.pr);
  return (
    <div className="overflow-hidden rounded-md border-2 border-w-ink bg-w-field">
      <div className="flex items-center gap-2 border-b-2 border-w-ink bg-w-sand px-2.5 py-1.5 font-mono text-w-xs text-w-text">
        <FileText size={13} weight="bold" className="shrink-0" />
        <span className="min-w-0 flex-1 truncate" title={draft.path ?? ""}>
          {draft.path}
        </span>
        {last && (
          <span className="shrink-0 text-w-muted">
            {draft.start_line ? `L${draft.start_line}-${last}` : `L${last}`}
          </span>
        )}
        {link && (
          <a
            href={link}
            target="_blank"
            rel="noopener noreferrer"
            onClick={(e) => e.stopPropagation()}
            className="inline-flex shrink-0 items-center gap-1 rounded-md border border-w-ink bg-w-field px-1.5 font-sans text-w-2xs font-semibold text-w-link hover:bg-w-aqua"
          >
            <GitPullRequest size={11} weight="bold" />
            {draft.pr}
          </a>
        )}
      </div>
      {rows.length > 0 ? (
        <div className="overflow-x-auto">
          <table className="w-full border-collapse font-mono text-w-xs leading-[1.45]">
            <tbody>
              {rows.map((r, i) => (
                <Fragment key={i}>
                  {r.kind === "hunk" ? (
                    <tr className="bg-w-card text-w-link">
                      <td colSpan={3} className="px-2 py-0.5">
                        {r.text}
                      </td>
                    </tr>
                  ) : (
                    <tr
                      className={cn(
                        r.kind === "add" && "bg-w-mint",
                        r.kind === "del" && "bg-w-rose",
                        marked(r) && "shadow-[inset_4px_0_0_rgb(var(--w-gold))]",
                      )}
                    >
                      <td className="w-10 select-none px-1.5 text-right text-w-muted">{r.old ?? ""}</td>
                      <td className="w-10 select-none px-1.5 text-right text-w-muted">{r.new ?? ""}</td>
                      <td className="whitespace-pre px-2 text-w-strong">
                        <span className={cn("mr-1 select-none", r.kind === "add" ? "text-w-ok" : r.kind === "del" ? "text-w-danger" : "text-transparent")}>
                          {r.kind === "add" ? "+" : r.kind === "del" ? "-" : " "}
                        </span>
                        {r.text}
                      </td>
                    </tr>
                  )}
                </Fragment>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="px-2.5 py-1.5 text-w-xs text-w-muted">No diff lines came with this comment.</p>
      )}
    </div>
  );
}
