"use client";

import { PaneResizer, usePaneWidth } from "./pane-resizer";
import { RelativeTime } from "./relative-time";
import { ownsQuest, ScopeToggle, useEveryone } from "./scope";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BellSimple, ClockCounterClockwise, CaretRight, Check, Flag, GitPullRequest } from "@phosphor-icons/react";
import { TodoNudgeActions } from "./todos-ui";
import { EventMark, eventGlyph } from "./event-glyphs";
import { cn } from "@/lib/utils";
import {
  formatUsage,
  matchesQuery,
  RUN_PAGE,
  CLI_OPTIONS,
  reviewerLabel,
  worldApi,
  type Notifications,
  type RunEvent,
  type SessionBrief,
  type SessionRun,
  type TaskBrief,
  type WorldAgent,
  type WorldEvent,
} from "@/lib/world";
import { BTN_PRIMARY_SM, BTN_SECONDARY, BTN_SECONDARY_SM, CHUNKY, FOCUS, TAB, TAB_OFF, TAB_ON, ITEM_CARD, NAV_OFF, NAV_ON, PANEL, PILL_DANGER, PILL_NEUTRAL, PILL_OK, SIDEBAR, TAG_X, UNREAD } from "./styles";
import { AgentFace } from "./channel-ui";
import { BoardColumn, BoardHint } from "./board-kit";
import { Keycap } from "./keycap";
import { milestoneProgress } from "./milestones-ui";
import { isDesktopApp } from "@/lib/desktop";
import { WCombobox, WSelect } from "./controls";
import { FilterField } from "./filter-field";
import { VoiceField } from "./voice-ui";
import { useDraft } from "./drafts";
import { CopyButton, DayDivider, MessageStamp, RichText, useAgentHandles } from "./chat-kit";
import { Shimmer } from "./work-ui";
import { openReport } from "@/lib/reports";
import { OpenWorkshop } from "./workbench/open-workshop";
import { AssigneeBadge, assigneeName, QUEST_STATUSES, SelectionBar, StatusIcon, statusLabel, useQuestControls, type QuestViewer } from "./quest-ui";
import { EmptyState, WindowHeader } from "./settings-shell";
import { runtimeName } from "@/lib/model-names";
import { escapeTaken } from "@/lib/escape";

export function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
}

export function runIsLive(run: SessionRun): boolean {
  return run.status === "running" || run.status === "queued";
}

export function useWorldRefresh(refresh: () => void, match: (e: WorldEvent) => boolean) {
  useEffect(() => {
    const onEvent = (e: Event) => {
      if (match((e as CustomEvent<WorldEvent>).detail)) refresh();
    };
    window.addEventListener("world-event", onEvent);
    window.addEventListener("world-reconnected", refresh);
    return () => {
      window.removeEventListener("world-event", onEvent);
      window.removeEventListener("world-reconnected", refresh);
    };
  }, [refresh, match]);
}

export const ROLE_LABEL: Record<string, string> = {
  main: "Main mind",
  task: "Executor",
  review: "Reviewer",
  aside: "Aside",
  designer: "Designer",
  direct: "Folder session",
};

export function Header({ title, subtitle, onClose }: { title: string; subtitle?: string; onClose: () => void }) {
  return (
    <WindowHeader title={title} subtitle={subtitle} onClose={onClose} />
  );
}

const BUBBLE = "max-w-[min(92%,46rem)] select-text rounded-lg border-2 border-w-ink px-3 py-2 text-w-base leading-relaxed";

interface RunRequest {
  from: string;
  text: string;
  notes: string[];
}

function between(text: string, start: string, end: string): string | null {
  const i = text.indexOf(start);
  if (i < 0) return null;
  const rest = text.slice(i + start.length);
  const j = end ? rest.indexOf(end) : -1;
  return (j < 0 ? rest : rest.slice(0, j)).trim();
}

function turnText(prompt: string): { text: string; refreshed: boolean } {
  const first = prompt.indexOf("[event]");
  const refreshed = first > 0 && /^\[(rules updated|instructions updated|memory handoff)\]/.test(prompt);
  const body = first >= 0 ? prompt.slice(first) : prompt;
  const parts = body.split(/\n\n(?=\[event\] )/).map((part) => {
    const msg = part.match(/^\[event\] message from ([^\n:]+):\n([\s\S]*)$/);
    if (msg) return `**${msg[1].trim()}:** ${msg[2].trim()}`;
    return part.replace(/^\[event\] /, "").trim();
  });
  return { text: parts.join("\n\n"), refreshed };
}

function runRequest(run: SessionRun, session: SessionBrief, agentName: string): RunRequest {
  const [base, ...retries] = run.prompt.split("\n\n[retry]\n");
  const notes: string[] = retries.length ? [retries.length > 1 ? `Retry ${retries.length}` : "Retry"] : [];
  const title = base.match(/^Task: (.*)$/m)?.[1]?.trim() ?? session.task_title ?? "";
  if (run.kind === "followup") {
    const m = base.match(/^\[follow-up from ([^\]\n]+)\]\n([\s\S]*?)\n\nAnswer briefly\./);
    if (m) return { from: m[1], text: m[2].trim(), notes };
  }
  if (run.kind === "direct") return { from: "You", text: base.trim(), notes };
  if (run.kind === "aside") {
    const asker = base.match(/^\[aside\] (.+?) pulled you aside/)?.[1];
    const question = between(base, "asks:\n", "\n\nThis is a side conversation");
    if (question !== null) return { from: asker ?? "Someone", text: question, notes };
  }
  if (run.kind === "task") {
    const description = between(base, `Task: ${title}\n`, "\n\nWorkspace:") ?? "";
    const brief = between(base, "\n\nYour brief:\n", "");
    if (base.includes("The independent reviewer asked for changes")) {
      const findings = between(base, "Address every finding:\n", "\nDo the work end to end") ?? "";
      return { from: "Reviewer", text: `Please fix the review findings on **${title}**.\n\n\`\`\`json\n${findings}\n\`\`\``, notes: ["Fix round", ...notes] };
    }
    const lines = [title && `**${title}**`, description, brief && `**Brief:** ${brief}`].filter(Boolean);
    return { from: "Quest", text: lines.join("\n\n"), notes };
  }
  if (run.kind === "review") {
    const focus = base.match(/^Focus especially on: (.*)$/m)?.[1]?.trim();
    const lines = [title ? `Review the work on **${title}**.` : "Review the work in this workspace.", focus && `**Focus:** ${focus}`].filter(Boolean);
    return { from: "Review request", text: lines.join("\n\n"), notes };
  }
  if (run.kind === "turn") {
    if (base.trim() === "/compact") return { from: "Runtime", text: `Compact ${agentName}'s memory.`, notes };
    const { text, refreshed } = turnText(base);
    return { from: "Inbox", text, notes: refreshed ? ["Instructions refreshed", ...notes] : notes };
  }
  return { from: "Prompt", text: base.trim(), notes };
}

export interface BlockCard {
  kind: string;
  data: Record<string, unknown> | null;
}

function splitBlocks(text: string): { text: string; cards: BlockCard[] } {
  const cards: BlockCard[] = [];
  const cleaned = text.replace(/<!--\s*(report|verdict|actions)\s*([\s\S]*?)\s*\1\s*-->/g, (_, kind: string, raw: string) => {
    let data: Record<string, unknown> | null = null;
    try {
      const parsed = JSON.parse(raw);
      data = parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : { items: parsed };
    } catch {
      data = null;
    }
    if (kind !== "actions") cards.push({ kind, data });
    return "";
  });
  return { text: cleaned.trim(), cards };
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

export function ReportCard({ card }: { card: BlockCard }) {
  const d = card.data ?? {};
  const findings = Array.isArray(d.findings) ? (d.findings as Record<string, unknown>[]) : [];
  const followups = Array.isArray(d.followups) ? (d.followups as unknown[]).map(str).filter(Boolean) : [];
  const badge = card.kind === "verdict" ? str(d.verdict) : str(d.status);
  return (
    <div className="mt-2 rounded-xl border-2 border-w-line bg-w-card/70 px-4 py-3 text-w-sm leading-relaxed">
      <p className="flex items-center gap-2 text-w-xs font-semibold text-w-muted">
        {card.kind === "verdict" ? "Verdict" : "Report"}
        {badge && (
          <span className={cn(badge === "approve" || badge === "done" ? PILL_OK : PILL_DANGER, "normal-case")}>
            {badge}
          </span>
        )}
      </p>
      {str(d.summary) && <p className="mt-1">{str(d.summary)}</p>}
      {findings.length > 0 && (
        <ul className="mt-1 ml-4 list-disc space-y-0.5">
          {findings.map((f, i) => (
            <li key={i}>
              <span className="font-semibold">{str(f.severity) || "note"}</span>
              {str(f.path) && <code className="mx-1 rounded-md bg-w-sand px-1 font-mono text-w-xs">{str(f.path)}</code>}
              {str(f.issue)}
            </li>
          ))}
        </ul>
      )}
      {followups.length > 0 && <p className="mt-2 text-w-xs text-w-muted"><span className="font-semibold text-w-text2">Left for later:</span> {followups.join("; ")}</p>}
    </div>
  );
}

type ThreadItem = { kind: "text"; body: string; id: string } | { kind: "error"; body: string; id: string } | { kind: "tools"; events: RunEvent[]; id: string };

function threadItems(run: SessionRun, events: RunEvent[] | undefined): ThreadItem[] {
  const items: ThreadItem[] = [];
  for (const e of events ?? []) {
    if (e.kind === "tool" || e.kind === "result") {
      const last = items[items.length - 1];
      if (last?.kind === "tools") last.events.push(e);
      else items.push({ kind: "tools", events: [e], id: `t${e.id}` });
    } else if (e.kind === "text" && e.body.trim()) {
      items.push({ kind: "text", body: e.body, id: `x${e.id}` });
    } else if (e.kind === "error") {
      items.push({ kind: "error", body: e.body, id: `e${e.id}` });
    }
  }
  const output = run.output?.trim();
  if (output) {
    const texts = items.filter((i): i is Extract<ThreadItem, { kind: "text" }> => i.kind === "text");
    const last = texts[texts.length - 1];
    if (!last || last.body.trim() !== output) {
      if (last && output.includes(last.body.trim())) items.splice(items.indexOf(last), 1);
      items.push({ kind: "text", body: output, id: "output" });
    }
  }
  if (run.error && !items.some((i) => i.kind === "error" && i.body === run.error)) items.push({ kind: "error", body: run.error, id: "run-error" });
  return items;
}

const ENTER = "animate-[fade-in-up_180ms_ease-out_both] motion-reduce:animate-none";

type ToolKind = "command" | "read" | "edit" | "search" | "web" | "helper" | "plan" | "other";

const TOOL_WORDS: Record<ToolKind, (n: number) => string> = {
  command: (n) => `ran ${n} ${n === 1 ? "command" : "commands"}`,
  read: (n) => `read ${n} ${n === 1 ? "file" : "files"}`,
  edit: (n) => `edited ${n} ${n === 1 ? "file" : "files"}`,
  search: (n) => `searched ${n} ${n === 1 ? "time" : "times"}`,
  web: (n) => `looked up ${n} ${n === 1 ? "page" : "pages"}`,
  helper: (n) => `asked ${n} ${n === 1 ? "helper" : "helpers"}`,
  plan: () => "updated the plan",
  other: (n) => `used ${n} ${n === 1 ? "tool" : "tools"}`,
};

function toolKind(tool: string | null): ToolKind {
  const t = (tool ?? "").toLowerCase();
  if (/^(bash|shell|exec|command|commandexecution|command_execution|run)$/.test(t)) return "command";
  if (/^(read|view|cat|notebookread)$/.test(t)) return "read";
  if (/^(edit|multiedit|write|delete|filechange|file_change|apply_patch|patch|notebookedit)$/.test(t)) return "edit";
  if (/^(grep|glob|search|find|ls|list)$/.test(t)) return "search";
  if (/^(webfetch|websearch|fetch|web_search)$/.test(t)) return "web";
  if (/^(agent|task)$/.test(t)) return "helper";
  if (/^(todowrite|todo|update_plan)$/.test(t)) return "plan";
  return "other";
}

function toolSummary(calls: RunEvent[]): string {
  const counts = new Map<ToolKind, number>();
  for (const call of calls) {
    const kind = toolKind(call.tool);
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  const order: ToolKind[] = ["command", "read", "edit", "search", "web", "helper", "plan", "other"];
  const text = order
    .filter((k) => counts.has(k))
    .map((k) => TOOL_WORDS[k](counts.get(k) ?? 0))
    .join(", ");
  return text ? text[0].toUpperCase() + text.slice(1) : "Worked";
}

function stepPhrase(tool: string | null, body: string): string {
  const file = body.trim().split("/").pop()?.replace(/"/g, "") ?? "";
  switch (toolKind(tool)) {
    case "read":
      return file ? `Reading ${file}` : "Reading files";
    case "edit":
      return file ? `Editing ${file}` : "Editing files";
    case "search":
      return "Searching the code";
    case "command": {
      const cmd = body.trim().split(/\s+/)[0] ?? "";
      if (cmd === "git") return "Working with git";
      if (["cargo", "npm", "pnpm", "yarn", "pytest", "go", "make"].includes(cmd)) return `Running ${cmd}`;
      return "Running a command";
    }
    case "web":
      return "Looking something up";
    case "helper":
      return "Asking a helper";
    case "plan":
      return "Planning the steps";
    default: {
      const name = (tool ?? "a tool").split("/").pop()?.split("__").pop() ?? "a tool";
      return `Using ${name}`;
    }
  }
}

function Reveal({ open, children }: { open: boolean; children: React.ReactNode }) {
  return (
    <div className={cn("grid transition-[grid-template-rows] duration-200 ease-out motion-reduce:transition-none", open ? "grid-rows-[1fr]" : "grid-rows-[0fr]")}>
      <div className="min-h-0 overflow-hidden">{children}</div>
    </div>
  );
}

interface ToolStep {
  call: RunEvent | null;
  result: RunEvent | null;
}

function pairSteps(events: RunEvent[]): ToolStep[] {
  const steps: ToolStep[] = [];
  for (const e of events) {
    if (e.kind === "tool") steps.push({ call: e, result: null });
    else {
      const open = [...steps].reverse().find((s) => s.call && !s.result && (!e.tool || s.call.tool === e.tool));
      if (open) open.result = e;
      else steps.push({ call: null, result: e });
    }
  }
  return steps;
}

function StepRow({ step }: { step: ToolStep }) {
  const [open, setOpen] = useState(false);
  const label = step.call ? `${step.call.tool ?? "tool"} ${step.call.body}`.trim() : "Output";
  const output = step.result?.body.trim() ?? "";
  return (
    <li className="rounded-md px-1.5 py-0.5">
      <button
        type="button"
        onClick={() => output && setOpen((o) => !o)}
        aria-expanded={output ? open : undefined}
        className={cn("flex w-full min-w-0 items-center gap-1.5 text-left font-mono text-w-2xs text-w-text2", output && "hover:text-w-strong")}
      >
        <span className={cn("shrink-0 transition-transform duration-150", open && "rotate-90", !output && "opacity-0")}>
          <CaretRight size={9} weight="bold" />
        </span>
        <span className="truncate">{label}</span>
      </button>
      {output && (
        <Reveal open={open}>
          <pre className="ml-4 mt-1 max-h-60 select-text overflow-auto whitespace-pre-wrap rounded-md bg-w-card p-2 font-mono text-w-2xs text-w-text2">{output}</pre>
        </Reveal>
      )}
    </li>
  );
}

function ToolGroup({ events }: { events: RunEvent[] }) {
  const [open, setOpen] = useState(false);
  const calls = events.filter((e) => e.kind === "tool");
  const steps = useMemo(() => pairSteps(events), [events]);
  return (
    <div className={cn("text-w-xs text-w-muted", ENTER)}>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="inline-flex max-w-full items-center gap-1.5 rounded-md px-1.5 py-0.5 transition-colors hover:bg-w-sand/70 hover:text-w-text2"
      >
        <span className="truncate">{toolSummary(calls)}</span>
        <span className={cn("shrink-0 transition-transform duration-200", open && "rotate-90")}>
          <CaretRight size={10} weight="bold" />
        </span>
      </button>
      <Reveal open={open}>
        <ol className="ml-1 mt-1 max-h-80 space-y-0.5 overflow-auto border-l-2 border-w-line pl-2">
          {steps.map((step, i) => (
            <StepRow key={step.call?.id ?? step.result?.id ?? i} step={step} />
          ))}
        </ol>
      </Reveal>
    </div>
  );
}

function Clamp({ long, fade = "from-w-card", children }: { long: boolean; fade?: string; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  if (!long) return <>{children}</>;
  return (
    <div>
      <div className={cn("relative transition-[max-height] duration-200 ease-out", open ? "max-h-[200rem]" : "max-h-64 overflow-hidden")}>
        {children}
        {!open && <div className={cn("pointer-events-none absolute inset-x-0 bottom-0 h-12 bg-gradient-to-t to-transparent", fade)} />}
      </div>
      <button type="button" onClick={() => setOpen((o) => !o)} className="mt-1 text-w-xs font-semibold text-w-link hover:underline">
        {open ? "Show less" : "Show more"}
      </button>
    </div>
  );
}

export interface Progress {
  text: string;
  at: number;
}

export function StatusLine({ text }: { text: string }) {
  return (
    <p role="status" aria-live="polite" className="flex items-center gap-2 px-1.5 text-w-xs text-w-text2">
      <span key={text} className="truncate animate-[fade-in-up_160ms_ease-out_both] motion-reduce:animate-none">
        <Shimmer>{text}</Shimmer>
      </span>
    </p>
  );
}

export function RunTurn({
  run,
  session,
  agentName,
  events,
  previous,
  handles,
  onOpenAgent,
  onRetry,
  progress,
}: {
  run: SessionRun;
  session: SessionBrief;
  agentName: string;
  events: RunEvent[] | undefined;
  previous?: string;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  onRetry?: (run: SessionRun) => Promise<void>;
  progress?: Progress | null;
}) {
  const [retrying, setRetrying] = useState(false);
  const [retryNote, setRetryNote] = useState<string | null>(null);
  const [retryFailed, setRetryFailed] = useState(false);
  const request = useMemo(() => runRequest(run, session, agentName), [run, session, agentName]);
  const items = useMemo(() => threadItems(run, events), [run, events]);
  const live = run.status === "running" || run.status === "queued";
  const statusText = useMemo(() => {
    if (run.status === "queued") return run.kind === "followup" ? "Waiting for the running work to take it" : run.kind === "direct" ? "Handing it to your Mac" : "Waiting for a free slot";
    const list = events ?? [];
    const latest = list[list.length - 1];
    const latestAt = latest ? Date.parse(latest.created_at) : 0;
    if (progress && progress.at >= latestAt) return progress.text;
    if (!latest) return `${agentName} is thinking`;
    if (latest.kind === "text") return `${agentName} is writing`;
    const call = [...list].reverse().find((e) => e.kind === "tool");
    return call ? stepPhrase(call.tool, call.body) : `${agentName} is thinking`;
  }, [run.status, events, progress, agentName]);
  const usage = formatUsage(run.cost_usd, run.input_tokens + run.cached_input_tokens + run.output_tokens);
  const retry = async () => {
    if (!onRetry || retrying) return;
    setRetrying(true);
    setRetryNote(null);
    setRetryFailed(false);
    try {
      await onRetry(run);
      setRetryNote("Retry queued. The new attempt will appear below.");
    } catch (error) {
      setRetryFailed(true);
      setRetryNote(error instanceof Error && error.message.startsWith("409 ") ? error.message.slice(4) : "Could not retry this run.");
    } finally {
      setRetrying(false);
    }
  };
  return (
    <>
      <DayDivider at={run.created_at} previous={previous} />
      <div className={cn(BUBBLE, "group ml-auto bg-w-teal/15", ENTER)}>
        <p className="mb-1 flex flex-wrap items-center gap-1.5 text-w-xs font-semibold text-w-link">
          {request.from}
          {request.notes.map((n) => (
            <span key={n} className="rounded-md bg-w-invert px-1.5 py-px text-w-3xs text-w-oninvert">
              {n}
            </span>
          ))}
        </p>
        <div className="max-h-80 overflow-y-auto">
          <RichText text={request.text || "(empty prompt)"} handles={handles} onOpenAgent={onOpenAgent} />
        </div>
        <div className="mt-1 flex items-center justify-end gap-2">
          <details className="mr-auto min-w-0 text-w-2xs text-w-text2">
            <summary className="cursor-pointer">Full prompt</summary>
            <pre className="mt-1 max-h-60 select-text overflow-auto whitespace-pre-wrap rounded-md bg-w-card p-2 font-mono text-w-2xs">{run.prompt}</pre>
          </details>
          <CopyButton text={request.text} />
          <MessageStamp createdAt={run.created_at} />
        </div>
      </div>
      {items.map((item, i) => {
        if (item.kind === "tools") return <ToolGroup key={item.id} events={item.events} />;
        if (item.kind === "error")
          return (
            <div key={item.id} role="alert" className={cn(BUBBLE, "whitespace-pre-wrap border-w-danger bg-w-rose text-w-sm text-w-danger", ENTER)}>
              <Clamp long={item.body.length > 600} fade="from-w-rose">
                {item.body}
              </Clamp>
            </div>
          );
        const { text, cards } = splitBlocks(item.body);
        const lastText = !items.slice(i + 1).some((n) => n.kind === "text");
        return (
          <div key={item.id} className={cn(BUBBLE, "group relative bg-w-card", ENTER)}>
            {text && <CopyButton text={text} className="absolute right-1.5 top-1.5 bg-w-card" />}
            {text && (
              <Clamp long={text.length > 1600}>
                <RichText text={text} handles={handles} onOpenAgent={onOpenAgent} />
              </Clamp>
            )}
            {cards.map((card, j) => (
              <ReportCard key={j} card={card} />
            ))}
            {lastText && !live && (
              <div className="mt-1 flex items-center justify-end gap-2">
                {usage && <span className="mr-auto font-mono text-w-2xs text-w-muted">{usage}</span>}
                <MessageStamp createdAt={run.finished_at ?? run.created_at} />
              </div>
            )}
          </div>
        );
      })}
      {run.steered_into && (
        <p className="text-w-xs text-w-text2">
          {run.interrupt ? "Stopped the step it was on and passed this into the running work." : "Passed into the running work."}
        </p>
      )}
      {live && <StatusLine text={statusText} />}
      {!live && run.status !== "completed" && !items.some((i) => i.kind === "error") && (
        <p className="text-w-xs text-w-danger">This run ended as {run.status.replace("_", " ")}.</p>
      )}
      {onRetry && run.retry_mode && (
        <div className="flex flex-wrap items-center gap-2 text-w-xs text-w-text2">
          <button type="button" onClick={() => void retry()} disabled={retrying} className={cn(CHUNKY, "bg-w-gold px-2 py-1 text-w-xs hover:bg-w-gold2")}>
            {retrying ? "Queuing..." : run.kind === "turn" ? "Retry reply" : run.retry_mode === "resume" ? "Resume work" : "Retry with saved files"}
          </button>
          <span>
            {run.retry_mode === "resume"
              ? "Continues in the same CLI session with its previous conversation context."
              : "Starts a new CLI session in the same workspace, with its saved files still available."}
          </span>
        </div>
      )}
      {retryNote && <p role={retryFailed ? "alert" : "status"} className={cn("text-w-xs", retryFailed ? "text-w-danger" : "text-w-text2")}>{retryNote}</p>}
    </>
  );
}

export function sessionTitle(session: SessionBrief, agentName?: string): string {
  if (session.title) return session.title;
  if (session.task_title) return session.role === "review" ? `Review: ${session.task_title}` : session.task_title;
  if (session.role === "main" && agentName) return `${agentName} main`;
  return ROLE_LABEL[session.role] ?? session.role;
}

export function SessionDetail({
  session,
  agents,
  onSessionRefresh,
  onOpenAgent,
  workshop = true,
}: {
  session: SessionBrief;
  agents: WorldAgent[];
  onSessionRefresh: () => void;
  onOpenAgent: (id: string) => void;
  workshop?: boolean;
}) {
  const [runs, setRuns] = useState<SessionRun[]>([]);
  const [events, setEvents] = useState<Record<string, RunEvent[]>>({});
  const [text, setText] = useDraft(`session:${session.id}`);
  const [reviewer, setReviewer] = useState("");
  const [reviewCli, setReviewCli] = useState("");
  const [reviewModel, setReviewModel] = useState("");
  const [focus, setFocus] = useState("");
  const [showReview, setShowReview] = useState(false);
  const [interrupt, setInterrupt] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const loaded = useRef<Set<string>>(new Set());
  const fetching = useRef<Set<string>>(new Set());
  const lastEvent = useRef<Record<string, number>>({});
  const texts = useRef<Map<string, { prompt: string; output: string | null; done: boolean }>>(new Map());
  const seeded = useRef(false);
  const handles = useAgentHandles(agents);
  const agent = agents.find((a) => a.id === session.agent_id);
  const agentName = agent?.name ?? "The agent";

  const [hasEarlier, setHasEarlier] = useState(false);
  const fromBottom = useRef(0);
  const loadEvents = useCallback((list: SessionRun[]) => {
    for (const run of list) {
      const live = runIsLive(run);
      if (loaded.current.has(run.id) || fetching.current.has(run.id)) continue;
      fetching.current.add(run.id);
      const after = lastEvent.current[run.id];
      worldApi
        .runEvents(run.id, after)
        .then((fresh) => {
          if (fresh.length) lastEvent.current[run.id] = fresh[fresh.length - 1].id;
          if (!live) loaded.current.add(run.id);
          if (after === undefined) setEvents((prev) => ({ ...prev, [run.id]: fresh }));
          else if (fresh.length)
            setEvents((prev) => {
              const have = prev[run.id] ?? [];
              const last = have.length ? have[have.length - 1].id : -1;
              return { ...prev, [run.id]: [...have, ...fresh.filter((e) => e.id > last)] };
            });
        })
        .catch(() => {})
        .finally(() => fetching.current.delete(run.id));
    }
  }, []);
  const fillText = useCallback((list: SessionRun[]) => {
    for (const run of list) {
      const known = texts.current.get(run.id);
      if (known && (known.done || runIsLive(run))) continue;
      worldApi
        .runText(run.id)
        .then(({ prompt, output }) => {
          texts.current.set(run.id, { prompt, output, done: !runIsLive(run) });
          setRuns((prev) => prev.map((r) => (r.id === run.id ? { ...r, prompt, output } : r)));
        })
        .catch(() => {});
    }
  }, []);
  const withText = (run: SessionRun): SessionRun => {
    const known = texts.current.get(run.id);
    return known ? { ...run, prompt: known.prompt, output: known.output } : run;
  };
  const remember = (list: SessionRun[]) => {
    for (const run of list) texts.current.set(run.id, { prompt: run.prompt, output: run.output, done: !runIsLive(run) });
  };
  const refresh = useCallback(() => {
    const light = seeded.current;
    const id = session.id;
    worldApi
      .sessionRuns(id, undefined, light)
      .then((page) => {
        if (id !== sessionRef.current) return;
        seeded.current = true;
        if (!light) remember(page);
        const next = page.map(withText);
        setRuns((prev) => {
          const first = next[0]?.created_at;
          const older = first ? prev.filter((run) => run.created_at < first) : [];
          if (!older.length) setHasEarlier(next.length >= RUN_PAGE);
          return [...older, ...next];
        });
        loadEvents(next);
        if (light) fillText(page);
      })
      .catch(() => {});
  }, [session.id, loadEvents, fillText]);
  const loadEarlier = () => {
    const first = runs[0]?.created_at;
    if (!first) return;
    worldApi
      .sessionRuns(session.id, first)
      .then((page) => {
        remember(page);
        setHasEarlier(page.length >= RUN_PAGE);
        setRuns((prev) => [...page.filter((run) => !prev.some((p) => p.id === run.id)), ...prev]);
        loadEvents(page);
      })
      .catch(() => {});
  };
  const sessionRef = useRef(session.id);
  useEffect(() => {
    sessionRef.current = session.id;
    seeded.current = false;
    texts.current = new Map();
    loaded.current = new Set();
    fetching.current = new Set();
    lastEvent.current = {};
    setRuns([]);
    setEvents({});
  }, [session.id]);
  const pending = useRef<number | null>(null);
  const refreshSoon = useCallback(() => {
    if (pending.current !== null) return;
    pending.current = window.setTimeout(() => {
      pending.current = null;
      refresh();
    }, 1200);
  }, [refresh]);
  useEffect(() => () => {
    if (pending.current !== null) window.clearTimeout(pending.current);
  }, []);
  useEffect(() => refresh(), [refresh]);
  const match = useCallback((e: WorldEvent) => e.agent_id === session.agent_id && (e.type.startsWith("run.") || e.type.startsWith("session.") || e.type.startsWith("review.")), [session.agent_id]);
  useWorldRefresh(refreshSoon, match);
  const pickedUp = useCallback((e: WorldEvent) => e.agent_id === session.agent_id && (e.type === "run.started" || e.type === "run.steered"), [session.agent_id]);
  useWorldRefresh(refresh, pickedUp);
  const liveRun = [...runs].reverse().find((r) => r.status === "running" || r.status === "queued");
  const runningRun = runs.find((r) => r.status === "running");
  const [stopping, setStopping] = useState<"step" | "run" | null>(null);
  const stop = async (mode: "step" | "run") => {
    if (!runningRun || stopping) return;
    if (mode === "run" && !window.confirm(`Stop ${agentName}'s run? The work so far stays, and you can resume it later.`)) return;
    setStopping(mode);
    try {
      await worldApi.stopRun(runningRun.id, mode);
      setNote(mode === "step" ? "Stopping the current step. It will carry on with the rest." : "Stopping the run.");
      refresh();
    } catch (error) {
      setNote(error instanceof Error ? error.message : "Could not stop it.");
    } finally {
      setStopping(null);
    }
  };
  const live = !!liveRun;
  const liveKind = liveRun?.kind ?? null;
  const [progress, setProgress] = useState<Progress | null>(null);
  useEffect(() => {
    if (!liveKind) {
      setProgress(null);
      return;
    }
    const onEvent = (ev: Event) => {
      const e = (ev as CustomEvent<WorldEvent>).detail;
      if (e.agent_id !== session.agent_id) return;
      const p = e.payload ?? {};
      if (e.type === "agent.progress" && typeof p.text === "string" && p.text.trim() && (!p.run_kind || p.run_kind === liveKind)) {
        setProgress({ text: p.text.trim(), at: Date.now() });
      } else if (e.type === "agent.status" && liveKind !== "turn" && typeof p.detail === "string" && p.detail.trim() && p.status !== "idle") {
        setProgress({ text: p.detail.trim(), at: Date.now() });
      }
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [liveKind, session.agent_id]);
  useEffect(() => {
    if (!live) return;
    const timer = window.setInterval(refresh, 15000);
    return () => window.clearInterval(timer);
  }, [live, refresh]);
  const eventCount = Object.values(events).reduce((n, list) => n + list.length, 0);
  useEffect(() => {
    const box = scrollRef.current;
    if (box) box.scrollTop = box.scrollHeight - box.clientHeight - fromBottom.current;
  }, [runs.length, eventCount]);

  const isMain = session.role === "main";
  const canTalk = session.status !== "archived" && (isMain || session.role === "task" || session.role === "review" || session.role === "direct");

  const send = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!text.trim() || sending) return;
    setSending(true);
    try {
      if (isMain) await worldApi.send(session.agent_id, text.trim());
      else await worldApi.followUp(session.id, text.trim(), interrupt && (session.running || !!runningRun));
      setText("");
      setInterrupt(false);
      setNote(null);
      refresh();
    } catch (error) {
      setNote(error instanceof Error && /^4\d\d /.test(error.message) ? error.message.slice(4) : isMain ? "Could not send that message." : "Could not send that follow-up.");
    } finally {
      setSending(false);
    }
  };

  const review = async () => {
    try {
      const pick = reviewer === "model" ? { cli: reviewCli || null, model: reviewModel.trim() || null } : { agentId: reviewer || null };
      await worldApi.review(session.id, pick, focus.trim() || null);
      setFocus("");
      setShowReview(false);
      setNote("Review requested. The verdict goes to the agent and shows up as a new reviewer session.");
      onSessionRefresh();
    } catch {
      setNote("Could not request a review for this session.");
    }
  };

  const retry = async (run: SessionRun) => {
    await worldApi.retryRun(run.id);
    refresh();
    onSessionRefresh();
  };

  const retryable = [...runs].reverse().find((run) => run.retryable)?.id;
  const usage = formatUsage(session.cost_usd, session.tokens);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="border-b-2 border-dashed border-w-line px-4 py-3">
        <div className="flex items-center gap-2">
          <h3 className="min-w-0 flex-1 truncate text-w-lg font-semibold text-w-strong">{sessionTitle(session, agent?.name)}</h3>
          {workshop && session.role === "direct" && <OpenWorkshop detail={{ sessionId: session.id, agentId: session.agent_id }} label="Workshop" />}
          {workshop && (session.role === "task" || session.role === "review") && (
            <OpenWorkshop detail={{ sessionId: session.role === "task" ? session.id : null, taskId: session.task_id, agentId: session.agent_id }} label="Workshop" />
          )}
        </div>
        <p className="mt-0.5 truncate text-w-xs text-w-text2">
          {agent ? `${agent.name} · ` : ""}
          {ROLE_LABEL[session.role] ?? session.role} on {runtimeName(session.backend, session.model)} · {session.status}
          {session.running ? " · working now" : ""}
          {usage ? ` · ${usage}` : ""}
        </p>
        {canTalk && !isMain && runningRun && (
          <div className="mt-2 flex flex-wrap gap-2">
            {(session.role === "task" && (runningRun.kind === "task" || runningRun.kind === "followup") || session.role === "direct" && runningRun.kind === "direct") && (
              <button
                type="button"
                onClick={() => void stop("step")}
                disabled={!!stopping}
                title="Stop the current step, keep the quest going"
                className={BTN_SECONDARY_SM}
              >
                {stopping === "step" ? "Stopping..." : "Stop step"}
              </button>
            )}
            <button
              type="button"
              onClick={() => void stop("run")}
              disabled={!!stopping}
              title="End this run now"
              className={cn(CHUNKY, "px-2.5 py-1 text-w-xs bg-w-alert text-w-onalert hover:bg-w-coral")}
            >
              {stopping === "run" ? "Stopping..." : "Stop run"}
            </button>
          </div>
        )}
        {session.summary && (
          <details className="mt-1 text-w-sm">
            <summary className="cursor-pointer text-w-xs font-medium text-w-muted">Last report</summary>
            <pre className="mt-1 max-h-48 select-text overflow-auto whitespace-pre-wrap rounded-md bg-w-card p-2 font-sans text-w-xs">{session.summary}</pre>
          </details>
        )}
      </div>
      <div
        ref={scrollRef}
        onScroll={(e) => {
          const box = e.currentTarget;
          const gap = box.scrollHeight - box.clientHeight - box.scrollTop;
          fromBottom.current = gap < 40 ? 0 : gap;
        }}
        className="min-h-0 flex-1 space-y-2 overflow-y-auto px-4 py-3"
      >
        {runs.length === 0 && <EmptyState icon={ClockCounterClockwise} title="Nothing has happened in this session yet" />}
        {hasEarlier && (
          <button type="button" onClick={loadEarlier} className="mx-auto block rounded-md border-2 border-w-line2 bg-w-field/70 px-3 py-1 text-w-xs text-w-text2 hover:border-w-ink">
            Show earlier
          </button>
        )}
        {runs.map((run, i) => (
          <RunTurn
            key={run.id}
            run={run}
            session={session}
            agentName={agentName}
            events={events[run.id]}
            previous={runs[i - 1]?.created_at}
            handles={handles}
            onOpenAgent={onOpenAgent}
            onRetry={run.id === retryable ? retry : undefined}
            progress={run.id === liveRun?.id ? progress : null}
          />
        ))}
      </div>
      {canTalk ? (
        <div className="space-y-2 border-t-2 border-dashed border-w-line2 bg-w-card/70 px-4 py-3">
          {showReview && session.role === "task" && (
            <div className="flex flex-wrap gap-2">
              <WSelect
                ariaLabel="Reviewer"
                className={cn(reviewer === "model" ? "w-40" : "w-56", "flex-none")}
                value={reviewer}
                onChange={setReviewer}
                options={[
                  { value: "", label: "Default reviewer", hint: agent ? reviewerLabel(agent.reviewer, agent) : undefined },
                  { value: "model", label: "Pick a model" },
                  ...agents.map((a) => ({ value: a.id, label: `${a.name}'s model`, hint: a.model })),
                ]}
              />
              {reviewer === "model" && (
                <>
                  <WSelect
                    ariaLabel="Reviewer CLI"
                    className="w-40 flex-none"
                    value={reviewCli}
                    onChange={setReviewCli}
                    options={[{ value: "", label: "Session's CLI", hint: session.backend }, ...CLI_OPTIONS]}
                  />
                  <div className="min-w-40 flex-1">
                    <WCombobox ariaLabel="Reviewer model" value={reviewModel} onChange={setReviewModel} placeholder="Model, e.g. fable" options={["fable", "opus", "sonnet"].map((m) => ({ value: m, label: m }))} />
                  </div>
                </>
              )}
              <div className="min-w-56 flex-1">
                <VoiceField placeholder="Review focus (optional)" ariaLabel="Review focus" value={focus} onChange={setFocus} />
              </div>
              <button type="button" onClick={() => void review()} className={cn(CHUNKY, "bg-w-gold px-3 text-w-sm hover:bg-w-gold2")}>
                Request review
              </button>
            </div>
          )}
          <form onSubmit={send} className="flex items-end gap-2">
            <VoiceField
              composer
              placeholder={isMain ? `Message ${agentName}'s main mind` : session.role === "review" ? "Ask this reviewer something" : session.role === "direct" ? (runningRun ? `Tell ${agentName} something while it works` : `Ask ${agentName} or tell it what to change`) : "Tell this executor something, or ask it a question"}
              ariaLabel="Follow-up"
              value={text}
              onChange={setText}
            />
            {(session.role === "task" && session.running || session.role === "direct" && !!runningRun) && !isMain && (
              <button
                type="button"
                onClick={() => setInterrupt((s) => !s)}
                aria-pressed={interrupt}
                title="Interrupt the current step first"
                className={cn(CHUNKY, "h-10 whitespace-nowrap px-3 text-w-sm", interrupt ? "bg-w-alert text-w-onalert" : "bg-w-card hover:bg-w-sand")}
              >
                Stop step
              </button>
            )}
            {session.role === "task" && (
              <button
                type="button"
                onClick={() => setShowReview((s) => !s)}
                aria-pressed={showReview}
                className={cn(CHUNKY, "h-10 px-3 text-w-sm", showReview ? "bg-w-gold" : "bg-w-card hover:bg-w-sand")}
              >
                Review
              </button>
            )}
            <button type="submit" disabled={!text.trim() || sending} className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-sm hover:bg-w-teal2 font-semibold text-w-strong")}>
              {sending ? "Sending..." : "Send"}
            </button>
          </form>
          {note && <p className="text-w-xs text-w-text2">{note}</p>}
        </div>
      ) : (
        note && <p className="px-4 pb-3 text-w-xs text-w-text2">{note}</p>
      )}
    </div>
  );
}

export type SessionFocus = { taskId?: string | null; sessionId?: string | null };

export function HistoryPanel({
  agents,
  initialAgentId,
  focus = null,
  onClose,
  onOpenAgent = () => {},
}: {
  agents: WorldAgent[];
  initialAgentId: string | null;
  focus?: SessionFocus | null;
  onClose: () => void;
  onOpenAgent?: (id: string) => void;
}) {
  useEscape(onClose);
  const [agentId, setAgentId] = useState<string>(initialAgentId ?? "");
  const [sessions, setSessions] = useState<SessionBrief[]>([]);
  const [selected, setSelected] = useState<string | null>(focus?.sessionId ?? null);
  const [missing, setMissing] = useState(false);
  const [listed, setListed] = useState(false);
  const focused = useRef(false);
  const refresh = useCallback(() => {
    worldApi
      .sessions(agentId || undefined)
      .then((list) => {
        setSessions(list);
        setListed(true);
        if (focused.current || !focus) return;
        focused.current = true;
        const pick =
          (focus.sessionId && list.find((x) => x.id === focus.sessionId)) ||
          (focus.taskId && (list.find((x) => x.task_id === focus.taskId && x.role === "task") ?? list.find((x) => x.task_id === focus.taskId)));
        if (pick) setSelected(pick.id);
        else setMissing(true);
      })
      .catch(() => {});
  }, [agentId, focus]);
  useEffect(() => refresh(), [refresh]);
  const match = useCallback((e: WorldEvent) => e.type.startsWith("task.") || e.type.startsWith("run.") || e.type.endsWith(".finished"), []);
  useWorldRefresh(refresh, match);
  const names = useMemo(() => new Map(agents.map((a) => [a.id, a.name])), [agents]);
  const agentNames = useMemo(() => new Map(agents.map((a) => [a.id, a.name])), [agents]);
  const current = sessions.find((s) => s.id === selected) ?? null;
  const [filter, setFilter] = useState("");
  const historyPane = usePaneWidth("history.list", 18, 13, 34);
  const shown = useMemo(
    () => sessions.filter((s) => matchesQuery(filter, sessionTitle(s, agentNames.get(s.agent_id)), agentNames.get(s.agent_id), ROLE_LABEL[s.role] ?? s.role)),
    [sessions, filter, agentNames],
  );

  return (
    <div role="dialog" aria-modal="true" aria-label="Session history" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden")}>
      <Header title="Session history" subtitle="Every main session, quest, review and aside." onClose={onClose} />
      <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
        <div
          style={historyPane.style}
          className={cn(
            "flex max-h-[35%] shrink-0 flex-col border-b-2 border-dashed border-w-line2 sm:max-h-none sm:border-b-0 sm:border-r-2",
            historyPane.custom ? "sm:w-[var(--pane-w)]" : "sm:w-[30%] sm:min-w-[15rem] sm:max-w-[22rem]",
            SIDEBAR,
          )}
        >
          <div className="space-y-2 p-3">
            <WSelect
              ariaLabel="Whose sessions"
              className="w-full"
              value={agentId}
              onChange={setAgentId}
              options={[{ value: "", label: "Everyone" }, ...agents.map((a) => ({ value: a.id, label: a.name }))]}
            />
            <FilterField value={filter} onChange={setFilter} placeholder="Filter sessions" />
          </div>
          <ul className="flex-1 space-y-0.5 overflow-y-auto px-2 pb-3">
            {!listed && <li className="px-3 py-4 text-w-sm"><Shimmer>Loading sessions</Shimmer></li>}
            {listed && filter && !shown.length && <li className="px-3 py-4 text-w-sm text-w-muted">No sessions match.</li>}
            {listed && !filter && !sessions.length && (
              <li className="px-3 py-4 text-w-sm text-w-muted">{agentId ? "No sessions for this agent yet. They appear once it replies or starts a quest." : "No sessions yet. They appear once an agent replies or starts a quest."}</li>
            )}
            {shown.map((s) => {
              const usage = formatUsage(s.cost_usd, s.tokens);
              return (
                <li key={s.id}>
                  <button
                    type="button"
                    onClick={() => setSelected(s.id)}
                    className={cn("w-full rounded-lg px-2.5 py-2.5 text-left text-w-sm transition-colors", selected === s.id ? NAV_ON : NAV_OFF)}
                  >
                    <span className="flex items-center gap-2">
                      <span className="min-w-0 flex-1 truncate font-semibold text-w-strong">{sessionTitle(s, agentNames.get(s.agent_id))}</span>
                      {s.running ? <Shimmer className="shrink-0 text-w-2xs">running</Shimmer> : <RelativeTime at={s.updated_at} className="shrink-0 text-w-2xs font-normal text-w-muted" />}
                    </span>
                    <span className="mt-1 flex min-w-0 items-center gap-1.5 text-w-xs font-normal text-w-muted">
                      <span className={cn(PILL_NEUTRAL, "shrink-0")}>{ROLE_LABEL[s.role] ?? s.role}</span>
                      <span className="truncate">{names.get(s.agent_id) ?? "?"} · {s.runs} {s.runs === 1 ? "run" : "runs"}{usage ? ` · ${usage}` : ""}</span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
        <PaneResizer pane={historyPane} edge="right" label="Resize the session list" className="hidden sm:block" />
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {current ? (
            <SessionDetail key={current.id} session={current} agents={agents} onSessionRefresh={refresh} onOpenAgent={onOpenAgent} />
          ) : (
            <div className="grid flex-1 place-items-center p-6">
              <EmptyState
                size="lg"
                icon={ClockCounterClockwise}
                title={missing ? "Not started yet" : "Pick a session"}
                description={
                  missing
                    ? "This quest has not started a session yet. It shows up here once its agent picks it up."
                    : "Read any session as a conversation, with every tool step one click away."
                }
              />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

const questCache: { list: TaskBrief[] } = { list: [] };

const COLUMN_EMPTY: Record<string, string> = {
  todo: "No quests waiting",
  in_progress: "Nobody is on a quest",
  review: "Nothing needs you",
  done: "Finished quests land here",
  failed: "Nothing stuck",
  cancelled: "Nothing cancelled",
};

export function QuestBoard({
  agents,
  viewer,
  onOpenAgent,
  onOpenQuest,
}: {
  agents: WorldAgent[];
  viewer: QuestViewer | null;
  onOpenAgent: (id: string) => void;
  onOpenQuest: (task: TaskBrief) => void;
}) {
  const [tasks, setTaskState] = useState<TaskBrief[]>(() => questCache.list);
  const setTasks = useCallback((next: TaskBrief[] | ((cur: TaskBrief[]) => TaskBrief[])) => {
    setTaskState((cur) => {
      const value = typeof next === "function" ? next(cur) : next;
      questCache.list = value;
      return value;
    });
  }, []);
  const [dragging, setDragging] = useState<string[] | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const refresh = useCallback(() => {
    worldApi.tasks().then(setTasks).catch(() => {});
  }, []);
  useEffect(() => refresh(), [refresh]);
  const match = useCallback((e: WorldEvent) => e.type.startsWith("task.") || e.type === "milestones.updated", []);
  useWorldRefresh(refresh, match);
  const [filter, setFilter] = useState("");
  const everyone = useEveryone();
  const byId = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);
  const others = useMemo(() => tasks.filter((t) => !ownsQuest(t, byId, viewer?.id)).length, [tasks, byId, viewer?.id]);
  const shown = useMemo(() => {
    return tasks.filter(
      (t) =>
        (everyone || ownsQuest(t, byId, viewer?.id)) &&
        matchesQuery(filter, t.title, t.repo, statusLabel(t.status), t.assigned_agent_id ? byId.get(t.assigned_agent_id)?.name : null),
    );
  }, [tasks, byId, filter, everyone, viewer?.id]);
  const columns = useMemo(
    () => QUEST_STATUSES.map((col) => ({ ...col, items: shown.filter((t) => col.match.includes(t.status)) })),
    [shown],
  );
  const order = useMemo(() => columns.flatMap((c) => c.items.map((t) => t.id)), [columns]);
  const q = useQuestControls({ tasks, setTasks, agents, viewer, order });
  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-3 px-4 pt-4 sm:px-5">
        <FilterField value={filter} onChange={setFilter} placeholder="Filter quests" className="w-full max-w-xs" />
        {filter && <span className="shrink-0 text-w-xs tabular-nums text-w-muted">{shown.length} of {tasks.length}</span>}
        <ScopeToggle others={others} className="ml-auto" />
      </div>
      <div className="flex min-h-0 flex-1 snap-x snap-mandatory gap-3 overflow-x-auto scroll-px-4 px-4 pb-4 pt-3 sm:snap-none sm:px-5 sm:scroll-px-5">
        {columns.map((col) => (
          <BoardColumn
            key={col.id}
            label={col.label}
            count={col.items.length}
            icon={<StatusIcon status={col.id} />}
            over={over === col.id}
            compact={(col.id === "cancelled" || col.id === "failed") && !col.items.length && over !== col.id}
            empty={COLUMN_EMPTY[col.id]}
            onDragOver={(e) => {
              if (!dragging) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              if (over !== col.id) setOver(col.id);
            }}
            onDragLeave={(e) => {
              if (!e.currentTarget.contains(e.relatedTarget as Node)) setOver((o) => (o === col.id ? null : o));
            }}
            onDrop={(e) => {
              e.preventDefault();
              const ids = dragging ?? [];
              setDragging(null);
              setOver(null);
              const moving = ids.filter((id) => tasks.find((t) => t.id === id && !col.match.includes(t.status)));
              if (moving.length) void q.apply(moving, { status: col.id });
            }}
          >
              {col.items.map((t) => {
                const a = t.assigned_agent_id ? q.byId.get(t.assigned_agent_id) : undefined;
                const done = t.status === "done";
                const canEdit = q.editable(t);
                const picked = q.selected.has(t.id);
                const progress = milestoneProgress(t.milestones);
                const merge = t.milestones?.find((m) => m.kind === "merge" && m.status === "active");
                return (
                  <li key={t.id}>
                    <div
                      data-quest={t.id}
                      tabIndex={0}
                      draggable={canEdit}
                      onDragStart={(e) => {
                        const ids = picked ? [...q.selected] : [t.id];
                        e.dataTransfer.effectAllowed = "move";
                        e.dataTransfer.setData("text/plain", ids.join(","));
                        setDragging(ids);
                      }}
                      onDragEnd={() => {
                        setDragging(null);
                        setOver(null);
                      }}
                      onMouseMove={() => q.hover(t.id)}
                      onMouseLeave={() => q.hover(null)}
                      onFocus={() => q.hover(t.id)}
                      onBlur={() => q.hover(null)}
                      onClickCapture={(e) => {
                        if (!(e.shiftKey || e.metaKey || e.ctrlKey) || !canEdit) return;
                        e.preventDefault();
                        e.stopPropagation();
                        q.toggle(t.id, e.shiftKey);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && e.target === e.currentTarget && a) onOpenQuest(t);
                      }}
                      className={cn(
                        ITEM_CARD,
                        "group relative px-3 py-2.5 text-w-sm outline-none focus-visible:ring-2 focus-visible:ring-w-teal",
                        picked && "border-w-teal bg-w-aqua/70 hover:border-w-teal",
                        dragging?.includes(t.id) && "opacity-50",
                        canEdit && "cursor-grab active:cursor-grabbing",
                      )}
                    >
                      <div className="flex items-start gap-2">
                        {canEdit ? (
                          <button
                            type="button"
                            data-quest-status={t.id}
                            onClick={(e) => q.open("status", t.id, e.currentTarget)}
                            title={`${statusLabel(t.status)}. Change status (S)`}
                            aria-label={`Status: ${statusLabel(t.status)}. Change status`}
                            className="-m-0.5 mt-px grid shrink-0 place-items-center rounded-md p-0.5 transition-colors hover:bg-w-sand"
                          >
                            <StatusIcon status={t.status} />
                          </button>
                        ) : (
                          <span className="mt-[3px] shrink-0" title={statusLabel(t.status)}>
                            <StatusIcon status={t.status} />
                          </span>
                        )}
                        <button
                          type="button"
                          onClick={() => onOpenQuest(t)}
                          title="Open the quest"
                          className="min-w-0 flex-1 rounded text-left outline-none focus-visible:ring-2 focus-visible:ring-w-teal disabled:cursor-default"
                          disabled={!a}
                        >
                          <span className="block font-semibold leading-snug text-w-strong">{t.title}</span>
                          {progress && (
                            <span className="mt-1 flex items-center gap-1.5 text-w-xs text-w-muted">
                              <Flag size={11} weight="bold" className="shrink-0" />
                              <span className="truncate">
                                {progress}
                                {merge ? ` · ${merge.detail ?? "waiting for review"}` : ""}
                              </span>
                            </span>
                          )}
                          {done && t.result_content && <span className="mt-1.5 line-clamp-2 whitespace-pre-wrap text-w-xs leading-relaxed text-w-text2">{t.result_content}</span>}
                        </button>
                        {canEdit && (
                          <button
                            type="button"
                            onClick={() => q.toggle(t.id)}
                            aria-label={picked ? "Deselect quest" : "Select quest"}
                            aria-pressed={picked}
                            title="Select (X)"
                            className={cn(
                              "mt-[3px] grid h-4 w-4 shrink-0 place-items-center rounded-[5px] border-2 transition-opacity",
                              picked ? "border-w-teal bg-w-teal text-w-strong opacity-100" : "border-w-line2 bg-w-field opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 [@media(pointer:coarse)]:hidden",
                              q.selected.size > 0 && "opacity-100 [@media(pointer:coarse)]:grid",
                            )}
                          >
                            {picked && <Check size={10} weight="bold" />}
                          </button>
                        )}
                      </div>
                      <div className="mt-2.5 flex items-center gap-2 text-w-xs text-w-text2">
                        {canEdit ? (
                          <button
                            type="button"
                            data-quest-assignee={t.id}
                            onClick={(e) => q.open("assignee", t.id, e.currentTarget)}
                            title={`${assigneeName(t, q.byId, q.people)}. Change assignee (A)`}
                            aria-label={`Assignee: ${assigneeName(t, q.byId, q.people)}. Change assignee`}
                            className="grid shrink-0 place-items-center rounded-md transition-opacity hover:opacity-80"
                          >
                            <AssigneeBadge task={t} byId={q.byId} people={q.people} />
                          </button>
                        ) : (
                          <span className="grid shrink-0 place-items-center">
                            <AssigneeBadge task={t} byId={q.byId} people={q.people} />
                          </span>
                        )}
                        {a ? (
                          <button type="button" onClick={() => onOpenAgent(a.id)} className="min-w-0 truncate font-medium transition-colors hover:text-w-link">
                            {a.name}
                          </button>
                        ) : (
                          <span className="min-w-0 truncate text-w-muted">{assigneeName(t, q.byId, q.people)}</span>
                        )}
                        {t.pr_url && (
                          <a
                            href={t.pr_url}
                            target="_blank"
                            rel="noopener noreferrer"
                            onClick={(e) => e.stopPropagation()}
                            title="Open the pull request"
                            className={cn(PILL_OK, "ml-auto shrink-0 py-1 transition-colors hover:border-w-ok")}
                          >
                            <GitPullRequest size={11} weight="bold" />
                            PR
                          </a>
                        )}
                      </div>
                    </div>
                  </li>
                );
              })}
          </BoardColumn>
        ))}
      </div>
      <BoardHint>
        <span className="flex items-center gap-1.5">Hover a quest, then</span>
        <span className="flex items-center gap-1.5"><Keycap size="sm">S</Keycap> status</span>
        <span className="flex items-center gap-1.5"><Keycap size="sm">A</Keycap> assignee</span>
        <span className="flex items-center gap-1.5"><Keycap size="sm">X</Keycap> select</span>
        <span className="flex items-center gap-1.5"><Keycap size="sm">Shift</Keycap> click for a range, drag to move</span>
      </BoardHint>
      <SelectionBar
        count={q.selected.size}
        onStatus={(el) => q.open("status", null, el)}
        onAssignee={(el) => q.open("assignee", null, el)}
        onClear={q.clear}
      />
      {q.overlay}
    </div>
  );
}

function NoteIcon({ type, error, agent }: { type: string; error: boolean; agent?: WorldAgent }) {
  if (type === "slack.notification")
    return (
      <span role="img" aria-label="Slack" className="mt-px grid h-9 w-9 shrink-0 place-items-center rounded-lg border-2 border-w-ink bg-w-card shadow-[0_2px_0_rgb(var(--w-ink))]">
        <SlackMark size={15} />
      </span>
    );
  return <EventMark glyph={eventGlyph(type, error)} agent={agent} size={36} />;
}

export function describeNotification(e: WorldEvent, names: Map<string, string>): string {
  const who = (e.agent_id && names.get(e.agent_id)) || "Someone";
  const p = e.payload as Record<string, unknown>;
  const str = (k: string) => (typeof p[k] === "string" ? (p[k] as string) : "");
  switch (e.type) {
    case "message.created":
      return `${who}: ${str("body").slice(0, 160)}`;
    case "task.finished": {
      if (str("error")) return `${who}'s quest needs attention: ${str("error")}`;
      const report = p.report as Record<string, unknown> | undefined;
      const summary = typeof report?.summary === "string" ? report.summary : "";
      return `${who} finished a quest${str("pr_url") ? " and opened a PR for you to review" : ""}. ${summary.slice(0, 140)}`;
    }
    case "task.merged":
      return `${who}'s quest is done: the PR for "${str("title")}" was merged`;
    case "task.pr_closed":
      return `The PR for ${who}'s quest "${str("title")}" was closed without merging`;
    case "run.failed":
    case "run.timed_out":
      return `${who} ran into trouble: ${str("error")}`;
    case "aside.answered":
      return `${who} answered your aside: ${str("answer").slice(0, 140)}`;
    case "agent.status":
      return `${who} is stuck: ${str("detail") || "an error stopped them"}`;
    case "timer.done":
      return `Timer done: ${str("label")}`;
    case "report.created":
      return `${who} wrote a report: ${str("title")}`;
    case "todo.nudge":
      if (p.timer) return `Time's up: ${str("text")}`;
      return `${p.overdue ? "Overdue" : "Still open"}: ${str("text")}${str("age") ? ` (${str("age")} old${str("added_by") ? `, from ${str("added_by")}` : ""})` : ""}`;
    case "agent.transferred": {
      const keys = Array.isArray(p.wanted_keys) ? (p.wanted_keys as string[]) : [];
      return `${str("by") || "Someone"} handed ${who} over to you.${keys.length ? ` Add ${keys.join(", ")} in You > Environment.` : ""}`;
    }
    case "attention.requested":
      return `${who} needs your decision: ${str("text")}`;
    case "agent.budget":
      return p.paused ? `${who} paused for today: ${str("text")}` : `${who} is near its daily budget: ${str("text")}`;
    case "channel.message":
      return `${str("author")} mentioned you in ${str("channel")}: ${str("body").slice(0, 140)}`;
    case "slack.notification": {
      const author = str("author") || "Someone";
      const where = str("kind") === "dm" ? "Slack" : `#${str("channel_name")} on Slack`;
      return `${author} in ${where}: ${str("text").slice(0, 160)}`;
    }
    default:
      return `${who}: ${e.type}`;
  }
}

export function SlackMark({ size = 16 }: { size?: number }) {
  return (
    <svg viewBox="0 0 54 54" width={size} height={size} aria-hidden>
      <g fill="none" fillRule="evenodd">
        <path d="M19.712.133a5.381 5.381 0 0 0-5.376 5.387 5.381 5.381 0 0 0 5.376 5.386h5.376V5.52A5.381 5.381 0 0 0 19.712.133m0 14.365H5.376A5.381 5.381 0 0 0 0 19.884a5.381 5.381 0 0 0 5.376 5.387h14.336a5.381 5.381 0 0 0 5.376-5.387 5.381 5.381 0 0 0-5.376-5.386" fill="#36C5F0" />
        <path d="M53.76 19.884a5.381 5.381 0 0 0-5.376-5.386 5.381 5.381 0 0 0-5.376 5.386v5.387h5.376a5.381 5.381 0 0 0 5.376-5.387m-14.336 0V5.52A5.381 5.381 0 0 0 34.048.133a5.381 5.381 0 0 0-5.376 5.387v14.364a5.381 5.381 0 0 0 5.376 5.387 5.381 5.381 0 0 0 5.376-5.387" fill="#2EB67D" />
        <path d="M34.048 54a5.381 5.381 0 0 0 5.376-5.387 5.381 5.381 0 0 0-5.376-5.386h-5.376v5.386A5.381 5.381 0 0 0 34.048 54m0-14.365h14.336a5.381 5.381 0 0 0 5.376-5.386 5.381 5.381 0 0 0-5.376-5.387H34.048a5.381 5.381 0 0 0-5.376 5.387 5.381 5.381 0 0 0 5.376 5.386" fill="#ECB22E" />
        <path d="M0 34.249a5.381 5.381 0 0 0 5.376 5.386 5.381 5.381 0 0 0 5.376-5.386v-5.387H5.376A5.381 5.381 0 0 0 0 34.25m14.336-.001v14.364A5.381 5.381 0 0 0 19.712 54a5.381 5.381 0 0 0 5.376-5.387V34.25a5.381 5.381 0 0 0-5.376-5.387 5.381 5.381 0 0 0-5.376 5.387" fill="#E01E5A" />
      </g>
    </svg>
  );
}

function notificationGroup(e: WorldEvent, names: Map<string, string>): string {
  if (e.type === "todo.nudge") return `todo:${String(e.payload.todo_id)}`;
  if (e.agent_id && typeof e.payload.pr_url !== "string") {
    if (e.type === "task.finished" && e.payload.error) return `${e.agent_id}:quest-error`;
    if (["agent.status", "run.failed", "run.timed_out"].includes(e.type)) return `${e.agent_id}:${e.type}`;
  }
  return `${e.type}:${e.agent_id ?? ""}:${describeNotification(e, names)}`;
}


export function NotificationsPanel({
  notes,
  agents,
  onOpenAgent,
  onChanged,
  onClose,
}: {
  notes: Notifications;
  agents: WorldAgent[];
  onOpenAgent: (id: string) => void;
  onChanged: () => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  const [view, setView] = useState<"open" | "cleared">("open");
  const [history, setHistory] = useState<Notifications | null>(null);
  const [gone, setGone] = useState<Set<number>>(new Set());
  useEffect(() => {
    if (view !== "cleared") return;
    let live = true;
    worldApi
      .notifications(true)
      .then((n) => live && setHistory(n))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [view]);
  useEffect(() => setGone(new Set()), [notes]);
  const shown = view === "open" ? notes : history;
  const names = useMemo(() => new Map(agents.map((a) => [a.id, a.name])), [agents]);
  const byId = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);
  const itemOf = useMemo(() => new Map((shown?.items ?? []).map((i) => [i.seq, i.id])), [shown]);
  const list = useMemo(() => {
    const events = (shown?.events ?? []).filter((e) => !gone.has(e.seq));
    const newest = view === "open" ? [...events].reverse() : [...events].sort((a, b) => (itemOf.get(b.seq) ?? 0) - (itemOf.get(a.seq) ?? 0));
    const groups = new Map<string, { event: WorldEvent; seqs: number[] }>();
    for (const event of newest) {
      const key = notificationGroup(event, names);
      const g = groups.get(key);
      if (g) g.seqs.push(event.seq);
      else groups.set(key, { event, seqs: [event.seq] });
    }
    return [...groups.values()];
  }, [shown, gone, names, view, itemOf]);
  const clear = (seqs: number[]) => {
    const ids = seqs.map((seq) => itemOf.get(seq)).filter((id): id is number => typeof id === "number");
    if (view !== "open" || !ids.length) return;
    setGone((prev) => new Set([...prev, ...seqs]));
    void worldApi.clearNotes({ ids }).then(onChanged).catch(onChanged);
  };
  const clearAll = () => {
    setGone(new Set((notes.events ?? []).map((e) => e.seq)));
    void worldApi.clearNotes({ all: true }).then(onChanged).catch(onChanged);
  };
  const open = (e: WorldEvent, seqs: number[]) => {
    clear(seqs);
    if (e.type === "report.created" && typeof e.payload.report_id === "string") openReport(e.payload.report_id);
    else if (e.type === "channel.message" && typeof e.payload.channel_id === "string")
      window.dispatchEvent(new CustomEvent("world:open-channel", { detail: { id: e.payload.channel_id } }));
    else if (e.type === "todo.nudge" || e.type === "timer.done") window.dispatchEvent(new Event("world:open-lists"));
    else if (e.agent_id) onOpenAgent(e.agent_id);
  };
  const openCount = list.length && view === "open" ? list.reduce((n, g) => n + g.seqs.length, 0) : 0;
  return (
    <div role="dialog" aria-label="Notifications" className={cn(PANEL, "pointer-events-auto flex max-h-full w-full flex-col overflow-hidden")}>
      <WindowHeader
        title="Needs you"
        onClose={onClose}
        actions={
          openCount > 0 ? (
            <button type="button" onClick={clearAll} className={BTN_SECONDARY}>
              Clear all
            </button>
          ) : undefined
        }
      />
      <div role="tablist" className="flex gap-1 border-b-2 border-dashed border-w-line px-3">
        {(["open", "cleared"] as const).map((v) => (
          <button key={v} type="button" role="tab" aria-selected={view === v} onClick={() => setView(v)} className={cn(TAB, view === v ? TAB_ON : TAB_OFF)}>
            {v === "open" ? "Open" : "Cleared"}
          </button>
        ))}
      </div>
      <ul className="min-h-0 flex-1 divide-y divide-w-line overflow-y-auto">
        {list.length === 0 && (
          <li className="p-4">
            {view === "open" ? (
              <EmptyState icon={BellSimple} title="Nothing needs you" description="Questions, messages, mentions, reminders and failed quests wait here until you answer or open them." />
            ) : (
              <EmptyState icon={ClockCounterClockwise} title="Nothing cleared yet" description="Items you answer, open or clear move here." />
            )}
          </li>
        )}
        {list.map(({ event: e, seqs }, i) => {
          const pr = typeof e.payload.pr_url === "string" ? (e.payload.pr_url as string) : null;
          return (
            <li key={e.seq} style={i < 10 ? { animationDelay: `${i * 30}ms` } : undefined} className="group relative flex flex-wrap items-start gap-x-3 gap-y-2 px-4 py-3 transition-colors hover:bg-w-sand/40 motion-safe:animate-w-row-in">
              <NoteIcon type={e.type} error={typeof e.payload.error === "string" && !!e.payload.error} agent={e.agent_id ? byId.get(e.agent_id) : undefined} />
              <button type="button" onClick={() => open(e, seqs)} className="min-w-0 flex-1 whitespace-pre-wrap break-words text-left text-w-sm leading-relaxed text-w-text transition-colors hover:text-w-link">
                {describeNotification(e, names)}
                {seqs.length > 1 && <span className={cn(TAG_X, "ml-1.5 align-middle")}>x{seqs.length}</span>}
              </button>
              <div className="flex h-[1.625em] shrink-0 items-center gap-1.5 text-w-sm">
                <RelativeTime at={e.created_at} className="whitespace-nowrap text-w-2xs leading-none text-w-muted" />
                {view === "open" && (
                  <button type="button" onClick={() => clear(seqs)} title="Clear" aria-label="Clear" className={cn("-my-1 -mr-1 grid h-6 w-6 place-items-center rounded-md text-w-muted transition-colors hover:bg-w-mint hover:text-w-ok active:bg-w-ok/20", FOCUS, "focus-visible:ring-offset-0")}>
                    <Check size={14} weight="bold" />
                  </button>
                )}
                {pr && (
                  <a href={pr} target="_blank" rel="noopener noreferrer" className={BTN_PRIMARY_SM}>
                    PR
                  </a>
                )}
              </div>
              {view === "open" && e.type === "todo.nudge" && typeof e.payload.todo_id === "string" && (
                <div className="basis-full pl-12">
                  <TodoNudgeActions todoId={e.payload.todo_id} onOpen={() => window.dispatchEvent(new Event("world:open-lists"))} />
                </div>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

