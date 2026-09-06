"use client";

import { useEffect, useState } from "react";
import { ChatCircleText, CheckCircle, PaperPlaneTilt, Rocket, Sparkle } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type Todo, type TodoProposal, type WorldAgent } from "@/lib/world";
import { BTN_PRIMARY, BTN_PRIMARY_SM } from "./styles";
import { LinkedText } from "./review-draft";

const KIND_ICON = { reply: ChatCircleText, message: PaperPlaneTilt, task: Rocket, other: Sparkle } as const;

function same(a: TodoProposal, b: TodoProposal): boolean {
  const norm = (p: TodoProposal) => JSON.stringify([p.label ?? "", p.to ?? "", p.via ?? "", p.title ?? "", p.agent ?? "", p.repo ?? "", p.body.trim()]);
  return norm(a) === norm(b);
}

function agentName(todo: Todo, agents: WorldAgent[]): string {
  const id = todo.decision?.agent_id ?? todo.added_by_agent;
  return agents.find((a) => a.id === id)?.name ?? "your main agent";
}

export function ProposalStrip({ todo, agents, onDecided, onOpen }: { todo: Todo; agents: WorldAgent[]; onDecided: () => void; onOpen?: () => void }) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const proposals = todo.proposals ?? [];
  if (todo.decision) {
    return (
      <span className="mt-1 flex items-center gap-1 text-w-2xs font-semibold text-w-link">
        <CheckCircle size={11} weight="fill" className="shrink-0" />
        <span className="truncate">{todo.decision.picked && todo.decision.picked.length > 1 ? `Approved ${todo.decision.picked.length}: ${todo.decision.label}` : `Approved: ${todo.decision.label}`}</span>
      </span>
    );
  }
  if (!proposals.length || todo.status !== "open") return null;
  const first = proposals[0];
  const Icon = KIND_ICON[first.kind] ?? Sparkle;
  const choose = proposals.length > 1 && onOpen;
  const approve = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (choose) {
      onOpen();
      return;
    }
    setBusy(true);
    setFailed(false);
    try {
      await worldApi.decideTodo(todo.id, 0);
      onDecided();
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <span className="mt-2.5 flex items-center gap-2 rounded-lg bg-w-sand/70 py-1.5 pl-2.5 pr-1.5 text-w-xs font-medium text-w-text">
      <Icon size={13} weight="bold" className="shrink-0 text-w-warn" />
      <span className="line-clamp-2 min-w-0 flex-1 break-words leading-tight" title={`${agentName(todo, agents)} suggests: ${first.label}`}>
        {failed ? "Did not go through" : first.label}
      </span>
      {proposals.length > 1 && <span className="shrink-0 tabular-nums text-w-muted">+{proposals.length - 1} more</span>}
      <button
        type="button"
        disabled={busy}
        onClick={(e) => void approve(e)}
        title={choose ? `Pick which of the ${proposals.length} suggestions to approve` : `Approve and send to ${agentName(todo, agents)}`}
        className={cn(BTN_PRIMARY_SM, "shrink-0")}
      >
        {busy ? "..." : choose ? "Choose" : "Approve"}
      </button>
    </span>
  );
}

const FIELD = "w-full rounded-md border-2 border-w-line bg-w-field px-2 py-1 text-w-sm outline-none focus:border-w-ink";

export function ProposalPanel({ todo, agents, onDecided }: { todo: Todo; agents: WorldAgent[]; onDecided: () => void }) {
  const proposals = todo.proposals ?? [];
  const [pick, setPick] = useState(0);
  const [chosen, setChosen] = useState<Set<number>>(() => new Set([0]));
  const [drafts, setDrafts] = useState<TodoProposal[]>(proposals);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const key = JSON.stringify(proposals);
  useEffect(() => {
    setDrafts(JSON.parse(key) as TodoProposal[]);
    setPick(0);
    setChosen(new Set([0]));
  }, [key]);
  const suggester = agentName(todo, agents);
  if (todo.decision) {
    return (
      <div className="shrink-0 rounded-lg border-2 border-w-teal bg-w-aqua px-3 py-2 text-w-sm text-w-link">
        <p className="flex items-center gap-1.5 font-semibold">
          <CheckCircle size={14} weight="fill" />
          {todo.decision.picked && todo.decision.picked.length > 1 ? `Approved ${todo.decision.picked.length} actions` : `Approved: ${todo.decision.label}`}
          {todo.decision.edited && <span className="font-normal">(with your edits)</span>}
        </p>
        {todo.decision.picked && todo.decision.picked.length > 1 && (
          <ul className="mt-1 list-disc pl-5 text-w-xs">
            {todo.decision.picked.map((p) => (
              <li key={p.index}>{p.label}</li>
            ))}
          </ul>
        )}
        <p className="mt-0.5 text-w-xs">Sent to {suggester}, who takes it from here and strikes this off when done.</p>
      </div>
    );
  }
  if (!proposals.length || todo.status !== "open") return null;
  const current = drafts[pick] ?? proposals[pick];
  const original = proposals[pick];
  const edited = !same(current, original);
  const set = (patch: Partial<TodoProposal>) => setDrafts((all) => all.map((d, i) => (i === pick ? { ...d, ...patch } : d)));
  const many = proposals.length > 1;
  const picks = many ? [...chosen].sort((a, b) => a - b) : [pick];
  const draftOf = (i: number) => drafts[i] ?? proposals[i];
  const anyEdited = picks.some((i) => !same(draftOf(i), proposals[i]));
  const ready = picks.length > 0 && picks.every((i) => draftOf(i).body.trim());
  const toggle = (i: number) =>
    setChosen((cur) => {
      const next = new Set(cur);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  const approve = async () => {
    setBusy(true);
    setError(null);
    try {
      if (many) await worldApi.decideTodos(todo.id, picks.map((i) => ({ index: i, proposal: same(draftOf(i), proposals[i]) ? undefined : draftOf(i) })));
      else await worldApi.decideTodo(todo.id, pick, edited ? current : undefined);
      onDecided();
    } catch {
      setError("That did not go through, so nothing was sent. Try again.");
    } finally {
      setBusy(false);
    }
  };
  const dismiss = async () => {
    setBusy(true);
    try {
      await worldApi.clearTodoProposals(todo.id);
      onDecided();
    } catch {
      setError("Could not clear the suggestions.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="shrink-0 rounded-lg border-2 border-w-ink bg-w-card p-3 shadow-[0_2px_0_rgb(var(--w-ink))]">
      <p className="mb-2 flex items-center gap-1.5 text-w-xs font-semibold text-w-muted">
        <Sparkle size={12} weight="fill" className="text-w-warn" />
        {suggester} suggests
      </p>
      {many && (
        <>
          <p className="mb-1.5 text-w-xs text-w-muted">Tick every action to approve, untick to skip. Click one to read or edit it.</p>
          <div className="mb-2 flex flex-wrap gap-1" role="tablist" aria-label="Suggested actions">
            {proposals.map((p, i) => {
              const Icon = KIND_ICON[p.kind] ?? Sparkle;
              const on = chosen.has(i);
              return (
                <span
                  key={i}
                  className={cn(
                    "flex items-center rounded-md border-2 text-w-xs font-semibold transition-colors",
                    pick === i ? "border-w-ink bg-w-invert text-w-oninvert" : "border-w-line bg-w-field text-w-text2 hover:bg-w-sand",
                  )}
                >
                  <input
                    type="checkbox"
                    checked={on}
                    onChange={() => toggle(i)}
                    aria-label={`${on ? "Skip" : "Approve"} ${p.label}`}
                    className="ml-1.5 h-3.5 w-3.5 shrink-0 accent-w-teal"
                  />
                  <button type="button" role="tab" aria-selected={pick === i} onClick={() => setPick(i)} className={cn("flex items-center gap-1 px-1.5 py-0.5", !on && "opacity-60")}>
                    <Icon size={12} weight="bold" />
                    {p.label}
                    {!same(draftOf(i), p) && <span className="font-normal">(edited)</span>}
                  </button>
                </span>
              );
            })}
          </div>
        </>
      )}
      {proposals.length === 1 && <p className="mb-2 text-w-base font-semibold text-w-strong">{original.label}</p>}
      <div className="space-y-1.5">
        {(current.kind === "reply" || current.kind === "message") && (
          <label className="flex items-center gap-2 text-w-xs text-w-muted">
            <span className="w-10 shrink-0">To</span>
            <input className={FIELD} value={current.to ?? ""} onChange={(e) => set({ to: e.target.value })} placeholder="Who or where" />
            {current.via && <span className="shrink-0 rounded-md bg-w-sand px-1.5 py-0.5 text-w-2xs font-semibold text-w-warn">{current.via}</span>}
          </label>
        )}
        {current.kind === "task" && (
          <>
            <label className="flex items-center gap-2 text-w-xs text-w-muted">
              <span className="w-10 shrink-0">Quest</span>
              <input className={FIELD} value={current.title ?? ""} onChange={(e) => set({ title: e.target.value })} placeholder="Quest title" />
            </label>
            <label className="flex items-center gap-2 text-w-xs text-w-muted">
              <span className="w-10 shrink-0">For</span>
              <input className={FIELD} list={`todo-agents-${todo.id}`} value={current.agent ?? ""} onChange={(e) => set({ agent: e.target.value })} placeholder="Which agent" />
              <datalist id={`todo-agents-${todo.id}`}>
                {agents.map((a) => (
                  <option key={a.id} value={a.name} />
                ))}
              </datalist>
            </label>
          </>
        )}
        <textarea
          value={current.body}
          onChange={(e) => set({ body: e.target.value })}
          aria-label={current.kind === "task" ? "What the quest should do" : "Text to send"}
          rows={Math.min(10, Math.max(3, Math.ceil(current.body.length / 60)))}
          className={cn(FIELD, "resize-y leading-relaxed")}
        />
        {/https?:\/\//.test(current.body) && (
          <div className="rounded-md bg-w-field/70 px-2 py-1 text-w-xs text-w-text2">
            <LinkedText text={current.body.match(/https?:\/\/\S+/g)?.join("\n") ?? ""} />
          </div>
        )}
      </div>
      {error && (
        <p role="alert" className="mt-1.5 text-w-xs text-w-danger">
          {error}
        </p>
      )}
      <div className="mt-2 flex items-center gap-2">
        <button
          type="button"
          disabled={busy || !ready}
          onClick={() => void approve()}
          className={BTN_PRIMARY}
        >
          {busy ? "Sending..." : many ? (picks.length === 0 ? "Tick an action" : `Approve ${picks.length === proposals.length ? "all " : ""}${picks.length}${anyEdited ? " with edits" : ""}`) : edited ? "Approve with edits" : "Approve"}
        </button>
        {edited && (
          <button type="button" onClick={() => set(original)} className="text-w-xs text-w-muted underline hover:text-w-strong">
            Undo edits
          </button>
        )}
        <button type="button" disabled={busy} onClick={() => void dismiss()} className="ml-auto text-w-xs text-w-muted hover:text-w-danger">
          {many ? "Skip all" : "Not now"}
        </button>
      </div>
    </div>
  );
}
