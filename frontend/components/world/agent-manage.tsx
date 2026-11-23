"use client";

import { RelativeTime } from "./relative-time";
import { parseTokens, tokenText } from "@/lib/budgets";
import { useCallback, useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { qk } from "@/lib/query-keys";
import { Archive, ArrowCounterClockwise, Broom, Stack, Star, Globe, HandArrowUp, LockSimple, MagnifyingGlass, Plus, ShieldCheck, Trash, X, Rss, CalendarBlank, Plugs, BookOpen, ChartBar, GithubLogo, UsersThree } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { APPROVAL_MODES, QUEST_CONTEXTS, cliOptions, compactNumber, formatUsage, reviewerLabel, type AgentUsageRow, type BackendModels, POWERS, SUBSCRIPTION_EVENTS, worldApi, WorldApiError, type AgentFeed, type FeedSecret, type AgentSchedule, prActionState, type AgentWatch, type WorldEvent, type AgentUsage, type ApprovalMode, type QuestContext, type McpServer, type Person, type Skill, type WorldAgent } from "@/lib/world";
import { BTN, BTN_MD, BTN_PRIMARY, BTN_SECONDARY, BTN_SECONDARY_SM, CHUNKY, INPUT } from "./styles";
import { ConfirmDelete, WCombobox, WSelect } from "./controls";
import { VoiceField } from "./voice-ui";
import { SlackMark, useWorldRefresh } from "./panels";
import { modelName } from "@/lib/model-names";
import { EmptyState, SEARCH_ROW_FOCUS, SEARCH_ROW_RING } from "./settings-shell";
import { CornerCopy } from "./chat-kit";
import { CrewMix } from "./crew-ui";
import { ASKING_MODES, askingServed, cleanVariants, MAX_CREW } from "@/lib/quest-room-api";
import { mixProblem } from "@/lib/quest-board";
import type { Asking, MixSlot } from "@/lib/quest-board";

const LABEL = "mb-1.5 block text-w-sm font-medium text-w-text2";
const SECTION = "space-y-3 rounded-xl border-2 border-w-line bg-w-card/70 p-4";

const EMPTY_BACKENDS: Record<string, BackendModels> = {};
const EMPTY_STRINGS: string[] = [];
const EMPTY_FEEDS: AgentFeed[] = [];
const EMPTY_SCHEDULES: AgentSchedule[] = [];
const EMPTY_KIT: { mcp_servers: McpServer[]; skills: Skill[] } = { mcp_servers: [], skills: [] };
const EMPTY_SERVERS: McpServer[] = [];
const EMPTY_SKILLS: Skill[] = [];

export function normalizeTag(raw: string): string {
  return raw
    .trim()
    .toLowerCase()
    .replace(/^#/, "")
    .replace(/[^a-z0-9+.-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);
}

export function TagChips({
  tags,
  editable,
  onChange,
  small,
}: {
  tags: string[];
  editable: boolean;
  onChange?: (tags: string[]) => void;
  small?: boolean;
}) {
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const chip = cn(
    "inline-flex items-center gap-1 rounded-[0.3rem] border-[1.5px] border-w-line2 bg-w-field text-w-text",
    small ? "px-1.5 text-w-3xs" : "px-2 py-0.5 text-w-2xs",
  );
  const commit = () => {
    const parts = draft.split(/[,\s]+/).map(normalizeTag).filter(Boolean);
    if (parts.length) onChange?.(Array.from(new Set([...tags, ...parts])));
    setDraft("");
    setAdding(false);
  };
  return (
    <span className="inline-flex flex-wrap items-center gap-1.5">
      {tags.map((t) => (
        <span key={t} className={chip}>
          #{t}
          {editable && (
            <button
              type="button"
              aria-label={`Remove tag ${t}`}
              onClick={() => onChange?.(tags.filter((x) => x !== t))}
              className="-mr-1 rounded p-0.5 text-w-muted hover:bg-w-sand hover:text-w-danger"
            >
              <X size={9} weight="bold" />
            </button>
          )}
        </span>
      ))}
      {editable &&
        (adding ? (
          <input
            autoFocus
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commit();
              } else if (e.key === "Escape") {
                e.stopPropagation();
                setDraft("");
                setAdding(false);
              }
            }}
            placeholder="tag"
            aria-label="New tag"
            className="w-24 rounded-[0.3rem] border-[1.5px] border-w-ink bg-w-field px-2 py-0.5 text-w-2xs outline-none focus:ring-2 focus:ring-w-teal"
          />
        ) : (
          <button
            type="button"
            onClick={() => setAdding(true)}
            className="inline-flex items-center gap-0.5 rounded-[0.3rem] border-[1.5px] border-dashed border-w-faint px-2 py-0.5 text-w-2xs text-w-muted hover:border-w-ink hover:text-w-text"
          >
            <Plus size={9} weight="bold" /> tag
          </button>
        ))}
    </span>
  );
}

export function MainAgentRow({ agent, isMain, canMakeMain, onMakeMain }: { agent: WorldAgent; isMain: boolean; canMakeMain: boolean; onMakeMain: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  if (!isMain && !canMakeMain) return null;
  return (
    <section className={cn(SECTION, "flex items-center gap-3 space-y-0")}>
      <span className={cn("grid h-9 w-9 shrink-0 place-items-center rounded-lg", isMain ? "bg-w-lemon text-w-warn" : "bg-w-sand text-w-muted")}>
        <Star size={18} weight={isMain ? "fill" : "regular"} />
      </span>
      <span className="min-w-0 flex-1 text-w-sm text-w-text2">
        {isMain
          ? `${agent.name} is your main agent: P opens them, they live in the town hall, watch over Loka for you and answer you on your phone and watch.`
          : failed
            ? "Could not make them your main agent."
            : `Make ${agent.name} your main agent: P opens them, they move into the town hall and watch over Loka for you.`}
      </span>
      {!isMain && (
        <button
          type="button"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setFailed(false);
            onMakeMain().catch(() => setFailed(true)).finally(() => setBusy(false));
          }}
          className={cn(BTN, BTN_MD, "bg-w-gold hover:bg-w-gold2")}
        >
          {busy ? "Moving in" : "Make main agent"}
        </button>
      )}
    </section>
  );
}

export function ApprovalRow({ agent, canChange, onUpdated }: { agent: WorldAgent; canChange: boolean; onUpdated: (agent: WorldAgent) => void }) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  if (!canChange) return null;
  const mode = agent.approval_mode ?? "auto";
  const pick = async (next: ApprovalMode) => {
    if (next === mode) return;
    setBusy(true);
    setFailed(false);
    try {
      onUpdated(await worldApi.update(agent.id, { approval_mode: next }));
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  const fallback = agent.cli === "claude" ? "Haiku models have no safety check, so auto approve gives them full access." : agent.cli === "codex" ? null : `${agent.cli} has no safety check, so it always runs with full access.`;
  return (
    <section className={cn(SECTION, "flex flex-wrap items-center gap-3 space-y-0")}>
      <ShieldCheck size={20} weight="bold" className="text-w-teal" />
      <span className="min-w-0 flex-1 text-w-sm text-w-text2">
        {failed ? "Could not change approvals." : `How ${agent.name}'s quests get permission to run commands and edit files.`}
        {fallback && <span className="block text-w-xs text-w-muted">{fallback}</span>}
      </span>
      <WSelect
        ariaLabel="Approval mode"
        className="w-44"
        value={mode}
        disabled={busy}
        onChange={(v) => void pick(v as ApprovalMode)}
        options={APPROVAL_MODES.map((m) => ({ value: m.value, label: m.label, hint: m.hint }))}
      />
    </section>
  );
}

export function QuestContextRow({ agent, canChange, onUpdated }: { agent: WorldAgent; canChange: boolean; onUpdated: (agent: WorldAgent) => void }) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  if (!canChange) return null;
  const level = agent.quest_context ?? "lean";
  const pick = async (next: QuestContext) => {
    if (next === level) return;
    setBusy(true);
    setFailed(false);
    try {
      onUpdated(await worldApi.update(agent.id, { quest_context: next }));
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={cn(SECTION, "flex flex-wrap items-center gap-3 space-y-0")}>
      <Stack size={20} weight="bold" className="text-w-teal" />
      <span className="min-w-0 flex-1 text-w-sm text-w-text2">
        {failed ? "Could not change quest context." : `How much of ${agent.name}'s main chat a new quest starts with.`}
        <span className="block text-w-xs text-w-muted">{QUEST_CONTEXTS.find((c) => c.value === level)?.hint}.</span>
      </span>
      <WSelect
        ariaLabel="Quest context"
        className="w-44"
        value={level}
        disabled={busy}
        onChange={(v) => void pick(v as QuestContext)}
        options={QUEST_CONTEXTS.map((c) => ({ value: c.value, label: c.label, hint: c.hint }))}
      />
    </section>
  );
}

export function AskingRow({ agent, canChange, onUpdated }: { agent: WorldAgent; canChange: boolean; onUpdated: (agent: WorldAgent) => void }) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const saved = JSON.stringify(cleanVariants(agent.crew_variants ?? [], MAX_CREW));
  const [mix, setMix] = useState<MixSlot[]>(() => JSON.parse(saved) as MixSlot[]);
  useEffect(() => setMix(JSON.parse(saved) as MixSlot[]), [saved]);
  if (!canChange || !askingServed(agent)) return null;
  const mode: Asking = agent.asking ?? "off";
  const save = async (patch: { asking?: Asking; crew_variants?: MixSlot[] }) => {
    setBusy(true);
    setFailed(false);
    try {
      onUpdated(await worldApi.update(agent.id, patch));
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  const dirty = JSON.stringify(cleanVariants(mix, MAX_CREW)) !== saved;
  return (
    <section className={SECTION}>
      <div className="flex flex-wrap items-center gap-3">
        <UsersThree size={20} weight="bold" className="text-w-teal" />
        <span className="min-w-0 flex-1 text-w-sm text-w-text2">
          {failed ? "Could not change access." : `Who ${agent.name}'s quests ask when they are missing something.`}
          <span className="block text-w-xs text-w-muted">{ASKING_MODES.find((m) => m.value === mode)?.hint}. Only taste, keys, money or anything that can&apos;t be undone reaches you.</span>
        </span>
        <WSelect
          ariaLabel="Access"
          className="w-44"
          value={mode}
          disabled={busy}
          onChange={(v) => v !== mode && void save({ asking: v as Asking })}
          options={ASKING_MODES.map((m) => ({ value: m.value, label: m.label, hint: m.hint }))}
        />
      </div>
      {mode === "crew" && (
        <div className="space-y-2">
          <p className="text-w-xs text-w-muted">What each helper runs on by default. Loka picks a mix of the coding tools on your devices unless you choose. The fourth slot applies to quests with four helpers, and a quest can still change the mix in Start with a crew.</p>
          <CrewMix size={MAX_CREW} value={mix} agent={agent} onChange={setMix} disabled={busy} />
          <button type="button" disabled={busy || !dirty || !!mixProblem(mix, MAX_CREW)} onClick={() => void save({ crew_variants: cleanVariants(mix, MAX_CREW) })} className={cn(BTN, BTN_MD, "bg-w-gold hover:bg-w-gold2")}>
            {busy ? "Saving" : "Save"}
          </button>
        </div>
      )}
    </section>
  );
}

export function ReviewerRow({ agent, canChange, onUpdated }: { agent: WorldAgent; canChange: boolean; onUpdated: (agent: WorldAgent) => void }) {
  const [cli, setCli] = useState(agent.reviewer?.cli ?? "");
  const [model, setModel] = useState(agent.reviewer?.model ?? "");
  const backends: Record<string, BackendModels> = useQuery({ queryKey: qk.backends(), queryFn: () => worldApi.backends() }).data ?? EMPTY_BACKENDS;
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    setCli(agent.reviewer?.cli ?? "");
    setModel(agent.reviewer?.model ?? "");
  }, [agent.reviewer?.cli, agent.reviewer?.model]);
  if (!canChange) return null;
  const effectiveCli = cli || agent.cli;
  const suggestions = Array.from(new Set([...(backends[effectiveCli]?.models ?? []), ...(effectiveCli === "claude" ? ["fable", "opus", "sonnet", "haiku"] : [])]));
  const saved = agent.reviewer ?? null;
  const dirty = (saved?.cli ?? "") !== cli || (saved?.model ?? "") !== model.trim();
  const save = async (next: { cli?: string; model: string } | null) => {
    setBusy(true);
    setFailed(false);
    try {
      onUpdated(await worldApi.update(agent.id, { reviewer: next }));
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={SECTION}>
      <div className="flex items-start gap-3">
        <MagnifyingGlass size={20} weight="bold" className="mt-0.5 text-w-teal" />
        <span className="min-w-0 flex-1 text-w-sm text-w-text2">
          {failed ? "Could not change the reviewer." : `Who reviews ${agent.name}'s quests when a review is on. Now: ${reviewerLabel(saved, agent)}${saved ? "" : " (their own model)"}.`}
          <span className="block text-w-xs text-w-muted">Reviews start fresh, so any CLI on their device works. A quest can still pick its own reviewer.</span>
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <WSelect
          ariaLabel="Reviewer CLI"
          className="w-40"
          value={cli}
          disabled={busy}
          onChange={setCli}
          options={[{ value: "", label: "Quest's CLI", hint: agent.cli }, ...cliOptions("opencode" in backends, agent.reviewer?.cli)]}
        />
        <div className="min-w-40 flex-1">
          <WCombobox
            ariaLabel="Reviewer model"
            value={model}
            onChange={setModel}
            placeholder="Their own model"
            options={suggestions.map((m) => ({ value: m, label: modelName(m), hint: m === backends[effectiveCli]?.default_model ? "default" : backends[effectiveCli]?.labels?.[m] }))}
          />
        </div>
        <button
          type="button"
          disabled={busy || !dirty || !model.trim()}
          onClick={() => void save({ ...(cli ? { cli } : {}), model: model.trim() })}
          className={cn(BTN, BTN_MD, "bg-w-gold hover:bg-w-gold2")}
        >
          {busy ? "Saving" : "Save"}
        </button>
        {saved && (
          <button type="button" disabled={busy} onClick={() => void save(null)} className={BTN_SECONDARY}>
            Use own model
          </button>
        )}
      </div>
    </section>
  );
}

export function TidyHistoryRow({ agent, canTidy }: { agent: WorldAgent; canTidy: boolean }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (!canTidy) return null;
  const tidy = async () => {
    setBusy(true);
    try {
      const { removed } = await worldApi.tidyHistory(agent.id);
      setNote(removed ? `Removed ${removed} empty failed ${removed === 1 ? "turn" : "turns"}.` : "Nothing to tidy.");
    } catch {
      setNote("Could not tidy the history.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={cn(SECTION, "flex items-center gap-3 space-y-0")}>
      <Broom size={20} weight="bold" className="text-w-text2" />
      <span className="min-w-0 flex-1 text-w-sm text-w-text2">
        {note ?? `Clear ${agent.name}'s failed turns that did nothing, so their history loads fast. Replies, quests and memory stay.`}
      </span>
      <button type="button" disabled={busy} onClick={() => void tidy()} className={BTN_SECONDARY}>
        {busy ? "Tidying" : "Tidy history"}
      </button>
    </section>
  );
}

export function ResetContextRow({ agent, canReset }: { agent: WorldAgent; canReset: boolean }) {
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (!canReset) return null;
  const reset = async () => {
    setBusy(true);
    try {
      const { queued } = await worldApi.resetContext(agent.id);
      setNote(queued
        ? `Reset queued. ${agent.name} first rewrites their notes, then starts fresh. Old chat stays above the dashed line; quests, schedules and keys are untouched.`
        : "A reset is already queued.");
      setConfirm(false);
    } catch {
      setNote("Could not queue the reset.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={cn(SECTION, "flex items-center gap-3 space-y-0")}>
      <ArrowCounterClockwise size={20} weight="bold" className="text-w-text2" />
      <span className="min-w-0 flex-1 text-w-sm text-w-text2">
        {note ?? (confirm
          ? `Really reset ${agent.name}'s mind? Their notes get rewritten first, then the conversation memory starts over.`
          : `Start ${agent.name} with a fresh mind: notes are rewritten first, the chat memory resets, old messages stay visible.`)}
      </span>
      {confirm ? (
        <span className="flex shrink-0 gap-2">
          <button type="button" disabled={busy} onClick={() => void reset()} className={cn(BTN, BTN_MD, "bg-w-alert text-w-onalert hover:bg-w-coral")}>
            {busy ? "Resetting" : "Yes, reset"}
          </button>
          <button type="button" disabled={busy} onClick={() => setConfirm(false)} className={BTN_SECONDARY}>
            Cancel
          </button>
        </span>
      ) : (
        <button type="button" disabled={busy} onClick={() => setConfirm(true)} className={cn(BTN_SECONDARY, "shrink-0")}>
          Reset context
        </button>
      )}
    </section>
  );
}

export function PowersSection({ agent, onUpdated }: { agent: WorldAgent; onUpdated: (agent: WorldAgent) => void }) {
  const [revoked, setRevoked] = useState<string[]>(agent.revoked_powers ?? []);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setRevoked(agent.revoked_powers ?? []), [agent.revoked_powers]);
  const toggle = async (id: string) => {
    const next = revoked.includes(id) ? revoked.filter((p) => p !== id) : [...revoked, id];
    const previous = revoked;
    setRevoked(next);
    setError(null);
    try {
      onUpdated(await worldApi.update(agent.id, { revoked_powers: next }));
    } catch {
      setRevoked(previous);
      setError("Could not change that power.");
    }
  };
  return (
    <section className={SECTION}>
      <p className="text-w-sm font-semibold text-w-strong">Powers</p>
      <p className="text-w-xs text-w-muted">
        Every agent starts with every power. Switch one off to take it away. You can also ask any agent with Manage powers to do it for you.
      </p>
      <ul className="grid gap-1.5 sm:grid-cols-2">
        {POWERS.map((p) => {
          const on = !revoked.includes(p.id);
          return (
            <li key={p.id}>
              <button
                type="button"
                role="switch"
                aria-checked={on}
                onClick={() => void toggle(p.id)}
                className={cn(
                  "flex w-full items-start gap-2 rounded-md border px-2.5 py-1.5 text-left transition-colors",
                  on ? "border-w-teal bg-w-teal/10" : "border-w-line bg-w-field/70 opacity-70",
                )}
              >
                <span className={cn("mt-0.5 h-3.5 w-6 shrink-0 rounded-[0.25rem] border-2 border-w-ink p-px", on ? "bg-w-teal" : "bg-w-line")}>
                  <span className={cn("block h-full w-2 rounded-[1px] bg-w-field transition-transform duration-200", on && "translate-x-2")} />
                </span>
                <span className="min-w-0">
                  <span className="block text-w-sm font-medium">{p.label}</span>
                  <span className="block text-w-xs text-w-muted">{p.detail}</span>
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </section>
  );
}

const EVENT_TYPE = /^[a-z0-9_]+(\.[a-z0-9_]+)*\.?$/;

export function SubscriptionsSection({ agentId, agentName, canChange }: { agentId: string; agentName: string; canChange: boolean }) {
  const queryClient = useQueryClient();
  const subs: string[] = useQuery({ queryKey: qk.agent.subscriptions(agentId), queryFn: () => worldApi.subscriptions(agentId) }).data ?? EMPTY_STRINGS;
  const setSubs = (next: string[]) => queryClient.setQueryData(qk.agent.subscriptions(agentId), next);
  const [custom, setCustom] = useState("");
  const [error, setError] = useState<string | null>(null);
  const save = async (next: string[]) => {
    const previous = subs;
    setSubs(next);
    setError(null);
    try {
      setSubs(await worldApi.setSubscriptions(agentId, next));
    } catch {
      setSubs(previous);
      setError("Could not save subscriptions.");
    }
  };
  const toggle = (id: string) => void save(subs.includes(id) ? subs.filter((t) => t !== id) : [...subs, id]);
  const known = new Set(SUBSCRIPTION_EVENTS.map((e) => e.id));
  const extra = subs.filter((t) => !known.has(t));
  const addCustom = (e: React.FormEvent) => {
    e.preventDefault();
    const t = custom.trim().toLowerCase();
    if (!EVENT_TYPE.test(t)) {
      setError("Use an event type like task.merged, or a prefix ending in a dot like task.");
      return;
    }
    setCustom("");
    if (!subs.includes(t)) void save([...subs, t]);
  };
  return (
    <section className={SECTION}>
      <p className="text-w-sm font-semibold text-w-strong">Subscriptions</p>
      <p className="text-w-xs text-w-muted">
        {agentName} hears about these as they happen and decides whether they matter, instead of checking on a timer. Only events from your own agents, your Slack, your GitHub and your timers reach it.
      </p>
      <ul className="grid gap-1.5 sm:grid-cols-2">
        {SUBSCRIPTION_EVENTS.map((ev) => {
          const on = subs.includes(ev.id);
          return (
            <li key={ev.id}>
              <button
                type="button"
                role="switch"
                aria-checked={on}
                disabled={!canChange}
                onClick={() => toggle(ev.id)}
                className={cn(
                  "flex w-full items-start gap-2 rounded-md border px-2.5 py-1.5 text-left transition-colors disabled:cursor-default",
                  on ? "border-w-teal bg-w-teal/10" : "border-w-line bg-w-field/70 opacity-70",
                )}
              >
                <span className={cn("mt-0.5 h-3.5 w-6 shrink-0 rounded-[0.25rem] border-2 border-w-ink p-px", on ? "bg-w-teal" : "bg-w-line")}>
                  <span className={cn("block h-full w-2 rounded-[1px] bg-w-field transition-transform duration-200", on && "translate-x-2")} />
                </span>
                <span className="min-w-0">
                  <span className="flex items-center gap-1.5 text-w-sm font-medium">
                    {ev.id === "slack.notification" && <SlackMark size={13} />}
                    {ev.id === "github." && <GithubLogo size={13} weight="duotone" />}
                    {ev.label}
                  </span>
                  <span className="block text-w-xs text-w-muted">{ev.detail}</span>
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      {extra.length > 0 && (
        <ul className="flex flex-wrap gap-1.5">
          {extra.map((t) => (
            <li key={t} className="flex items-center gap-1 rounded-md border border-w-teal bg-w-teal/10 px-2 py-0.5 font-mono text-w-xs">
              {t}
              {canChange && (
                <button type="button" aria-label={`Unsubscribe from ${t}`} onClick={() => toggle(t)} className="rounded-md p-0.5 hover:bg-w-teal/20">
                  <X size={11} weight="bold" />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {canChange && (
        <form onSubmit={addCustom} className="flex gap-2">
          <input className={cn(INPUT, "font-mono")} placeholder="task.merged or task." value={custom} onChange={(e) => setCustom(e.target.value)} aria-label="Other event type" />
          <button type="submit" disabled={!custom.trim()} className={BTN_SECONDARY_SM}>
            <Plus size={12} weight="bold" />
            Add
          </button>
        </form>
      )}
      <WatchList agentId={agentId} canChange={canChange} />
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </section>
  );
}

function slackTarget(w: AgentWatch): string | null {
  const channel = typeof w.match.slack_channel_id === "string" ? w.match.slack_channel_id : "";
  if (!channel) return null;
  const place = w.note?.startsWith("#") ? w.note : channel;
  const root = typeof w.match.thread_root === "string" ? Number(w.match.thread_root) : NaN;
  if (Number.isFinite(root)) return `thread in ${place} from ${new Date(root * 1000).toLocaleDateString()}`;
  return place;
}

function watchKind(w: AgentWatch): string {
  if (w.event_type === "github.") return "all GitHub events";
  if (w.event_type === "slack.") return "new Slack messages";
  return w.event_type;
}

function watchTarget(w: AgentWatch): string {
  const slack = slackTarget(w);
  if (slack) return slack;
  const repo = typeof w.match.repo === "string" ? w.match.repo : "";
  if (repo && typeof w.match.number === "number") return `${repo}#${w.match.number}`;
  return repo || Object.entries(w.match).map(([k, v]) => `${k}=${String(v)}`).join(", ");
}

function WatchList({ agentId, canChange }: { agentId: string; canChange: boolean }) {
  const queryClient = useQueryClient();
  const watches = useQuery({ queryKey: qk.agent.watches(agentId), queryFn: () => worldApi.watches(agentId) }).data ?? [];
  const [failed, setFailed] = useState(false);
  if (watches.length === 0) return null;
  const remove = async (id: string) => {
    setFailed(false);
    try {
      await worldApi.removeWatch(agentId, id);
      void queryClient.invalidateQueries({ queryKey: qk.agent.watches(agentId) });
    } catch {
      setFailed(true);
    }
  };
  return (
    <div>
      <p className="mb-1 text-w-xs font-medium text-w-muted">Watching</p>
      <ul className="space-y-1">
        {watches.map((w) => (
          <li key={w.id} className="flex items-center gap-2 text-w-xs">
            {slackTarget(w) ? <SlackMark size={12} /> : null}
            <span className="min-w-0 flex-1 truncate">
              {w.note?.startsWith("https://") ? (
                <a href={w.note} target="_blank" rel="noopener noreferrer" className="font-mono underline decoration-w-line underline-offset-2 hover:text-w-strong">
                  {watchTarget(w)}
                </a>
              ) : (
                <span className="font-mono">{watchTarget(w)}</span>
              )}
              <span className="text-w-muted"> {watchKind(w)}</span>
              <span className="text-w-muted">{w.task_id ? ", for its quest until the PR merges or closes" : w.expires_at ? `, until ${new Date(w.expires_at).toLocaleDateString()}` : ""}</span>
            </span>
            {canChange && !w.task_id && (
              <button type="button" aria-label={`Stop watching ${watchTarget(w)}`} onClick={() => void remove(w.id)} className="rounded-md p-0.5 hover:bg-w-sand">
                <X size={11} weight="bold" />
              </button>
            )}
          </li>
        ))}
      </ul>
      {failed && <p role="alert" className="text-w-xs text-w-danger">Could not stop that watch.</p>}
    </div>
  );
}

function feedEvery(secs: number): string {
  return secs % 3600 === 0 ? `every ${secs / 3600}h` : `every ${Math.round(secs / 60)} min`;
}

const FEED_SENDERS = [
  { value: "github", label: "GitHub" },
  { value: "gitlab", label: "GitLab" },
  { value: "alertmanager", label: "Alertmanager" },
  { value: "grafana", label: "Grafana" },
  { value: "other", label: "Something else" },
];

const FEED_CHECKS = [
  { value: "secret", label: "Secret on every call" },
  { value: "address", label: "Sender address only" },
];

function splitLines(text: string): string[] {
  return text.split(/[\n,]+/).map((r) => r.trim()).filter(Boolean);
}

function splitRanges(text: string): string[] {
  return text.split(/[\s,]+/).map((r) => r.trim()).filter(Boolean);
}

function feedAuthLine(f: AgentFeed): { text: string; tone: "ok" | "warn" | "muted" } | null {
  if (f.kind !== "webhook") return null;
  if (f.auth_mode === "address") {
    return f.auth === "verified"
      ? { text: "Checked by sender address, no secret needed", tone: "ok" }
      : { text: "Checked by sender address, no call yet", tone: "muted" };
  }
  switch (f.auth) {
    case "verified":
      return { text: "Sender uses the secret", tone: "ok" };
    case "grace":
      return {
        text: `Needs the sender updated: it posts without the secret, which stops working ${f.auth_grace_until ? new Date(f.auth_grace_until).toLocaleDateString() : "soon"}`,
        tone: "warn",
      };
    case "refusing":
      return { text: "Refusing its sender: calls come without the secret. Add the secret to the sender.", tone: "warn" };
    default:
      return { text: "No verified call yet", tone: "muted" };
  }
}

function SecretBlock({ label, text }: { label: string; text: string }) {
  return (
    <div className="group/code relative">
      <p className="text-w-3xs text-w-muted">{label}</p>
      <pre className="mt-0.5 overflow-x-auto whitespace-pre rounded-md border border-w-line bg-w-field px-2.5 py-1.5 pr-10 font-mono text-w-xs text-w-text select-all">{text}</pre>
      <CornerCopy text={text} label={`Copy ${label.toLowerCase()}`} className="absolute right-1.5 top-5" />
    </div>
  );
}

function FeedSecretCard({ data, onDone }: { data: FeedSecret; onDone: () => void }) {
  if (!data.secret) {
    return (
      <div className="space-y-2 rounded-md border-2 border-w-teal bg-w-field/70 p-3">
        <p className="text-w-sm font-semibold text-w-strong">{data.feed.name} is ready</p>
        <p className="text-w-xs text-w-text2">{data.setup}</p>
        <SecretBlock label="Webhook URL" text={data.feed.webhook_url ?? ""} />
        <SecretBlock label="curl" text={data.examples.curl} />
        <button type="button" onClick={onDone} className={BTN_SECONDARY_SM}>
          Done
        </button>
      </div>
    );
  }
  return (
    <div className="space-y-2 rounded-md border-2 border-w-teal bg-w-field/70 p-3">
      <p className="text-w-sm font-semibold text-w-strong">Secret for {data.feed.name}</p>
      <p className="text-w-xs text-w-text2">
        This secret is shown only once and agents never see it. Copy it into the sender now. Every call must send it as a bearer token, a basic auth password, an X-Loka-Token header or an HMAC signature, or Loka refuses it. To see a secret again, rotate it.
      </p>
      {data.setup && <p className="text-w-xs text-w-text2">{data.setup}</p>}
      <SecretBlock label="Webhook URL" text={data.feed.webhook_url ?? ""} />
      <SecretBlock label="Secret" text={data.secret} />
      <SecretBlock label="curl" text={data.examples.curl} />
      {data.examples.alertmanager && <SecretBlock label="Alertmanager" text={data.examples.alertmanager} />}
      {data.examples.github && <SecretBlock label="GitHub webhook" text={data.examples.github} />}
      <button type="button" onClick={onDone} className={BTN_SECONDARY_SM}>
        I copied it
      </button>
    </div>
  );
}

function NewWebhookForm({ agentId, onMade, onCancel }: { agentId: string; onMade: (data: FeedSecret) => void; onCancel: () => void }) {
  const [name, setName] = useState("");
  const [sender, setSender] = useState("github");
  const [senderName, setSenderName] = useState("");
  const [check, setCheck] = useState("secret");
  const [ranges, setRanges] = useState("");
  const [keep, setKeep] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const custom = sender === "other";
  const byAddress = check === "address";
  const make = async () => {
    const source = custom ? senderName.trim() : sender;
    if (!source) {
      setError("Say what will send to this webhook.");
      return;
    }
    if (byAddress && splitRanges(ranges).length === 0) {
      setError("Add the addresses the sender posts from, or keep the secret.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      onMade(
        await worldApi.createWebhookFeed(agentId, {
          name: name.trim() || undefined,
          source,
          auth: byAddress ? "address" : "secret",
          allowed_ips: splitRanges(ranges),
          keep: custom && splitLines(keep).length ? splitLines(keep) : undefined,
        }),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not make the webhook.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-2 rounded-md border border-w-line bg-w-field/70 p-3">
      <p className="text-w-xs font-medium text-w-strong">What will send to it?</p>
      <div className="flex flex-wrap items-center gap-2">
        <WSelect ariaLabel="What sends to this webhook" className="w-44" value={sender} onChange={setSender} options={FEED_SENDERS} />
        {custom && (
          <input aria-label="Sender name" value={senderName} onChange={(e) => setSenderName(e.target.value)} placeholder="Its name, like Statuspage" className={cn(INPUT, "py-1.5 text-w-sm")} />
        )}
        <input aria-label="Webhook name" value={name} onChange={(e) => setName(e.target.value)} placeholder="Feed name, optional" className={cn(INPUT, "py-1.5 text-w-sm")} />
      </div>
      {custom && (
        <>
          <input
            aria-label="Fields the agent sees"
            value={keep}
            onChange={(e) => setKeep(e.target.value)}
            placeholder="Fields the agent sees, like status, incident.name"
            className={cn(INPUT, "w-full py-1.5 font-mono text-w-sm")}
          />
          <p className="text-w-3xs text-w-muted">Only these fields of each call reach the agent. Leave empty and the whole body does, cut to 1500 characters. Your agent can narrow it later from the fields the first call carries.</p>
        </>
      )}
      <p className="text-w-xs font-medium text-w-strong">How should Loka know it is really them?</p>
      <div className="flex flex-wrap items-center gap-2">
        <WSelect ariaLabel="How the sender is checked" className="w-52" value={check} onChange={setCheck} options={FEED_CHECKS} />
        <input
          aria-label="Allowed sender addresses"
          value={ranges}
          onChange={(e) => setRanges(e.target.value)}
          placeholder={byAddress ? "Sender addresses, like 203.0.113.0/24" : "Optional sender addresses"}
          className={cn(INPUT, "min-w-0 flex-1 py-1.5 font-mono text-w-sm")}
        />
      </div>
      <p className="text-w-3xs text-w-muted">
        {byAddress
          ? "For senders that cannot send a secret. Calls from these addresses are accepted as they are, and calls from anywhere else are refused. Anyone who can post from those addresses can reach the agent, so keep the ranges tight."
          : "Every call must carry the secret you see next. With addresses set, calls from elsewhere are refused even with the secret."}
      </p>
      <div className="flex gap-2">
        <button type="button" disabled={busy} onClick={() => void make()} className={BTN_SECONDARY_SM}>
          {busy ? "Making" : "Make webhook"}
        </button>
        <button type="button" onClick={onCancel} className={BTN_SECONDARY_SM}>
          Cancel
        </button>
      </div>
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </div>
  );
}

function FeedRanges({ agentId, feed, canChange, onSaved }: { agentId: string; feed: AgentFeed; canChange: boolean; onSaved: () => void }) {
  const ranges = feed.allowed_ips ?? [];
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(ranges.join(", "));
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    setError(null);
    try {
      await worldApi.setFeedRanges(agentId, feed.id, splitRanges(text));
      setEditing(false);
      onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not save those addresses.");
    }
  };
  if (!editing) {
    return (
      <span className="flex flex-wrap items-center gap-1.5 text-w-xs text-w-muted">
        <span className="truncate">{ranges.length ? `Only from ${ranges.join(", ")}` : "From any address with the secret"}</span>
        {canChange && (
          <button type="button" onClick={() => { setText(ranges.join(", ")); setEditing(true); }} className="underline decoration-dotted hover:text-w-strong">
            Change
          </button>
        )}
      </span>
    );
  }
  return (
    <span className="block space-y-1">
      <span className="flex items-center gap-1.5">
        <input
          aria-label="Allowed sender addresses"
          autoFocus
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
            if (e.key === "Escape") setEditing(false);
          }}
          placeholder="203.0.113.0/24, 2001:db8::/32"
          className={cn(INPUT, "py-1 font-mono text-w-xs")}
        />
        <button type="button" onClick={() => void save()} className={BTN_SECONDARY_SM}>
          Save
        </button>
      </span>
      {error && <span role="alert" className="block text-w-xs text-w-danger">{error}</span>}
    </span>
  );
}

export function FeedsSection({ agentId, agentName, canChange, focusId }: { agentId: string; agentName: string; canChange: boolean; focusId?: string }) {
  const queryClient = useQueryClient();
  const feedsQuery = useQuery({ queryKey: qk.agent.feeds(agentId), queryFn: () => worldApi.feeds(agentId) });
  const feeds: AgentFeed[] | null = feedsQuery.data ?? (feedsQuery.isError ? EMPTY_FEEDS : null);
  const [error, setError] = useState<string | null>(null);
  const [making, setMaking] = useState(false);
  const [shown, setShown] = useState<FeedSecret | null>(null);
  const [rotating, setRotating] = useState<string | null>(null);
  const refresh = () => void queryClient.invalidateQueries({ queryKey: qk.agent.feeds(agentId) });
  const act = async (run: () => Promise<unknown>, failure: string) => {
    setError(null);
    try {
      await run();
      refresh();
    } catch (e) {
      setError(e instanceof Error && e.message && !e.message.startsWith("HTTP") ? e.message : failure);
    }
  };
  const show = (run: () => Promise<FeedSecret>, failure: string) =>
    act(async () => {
      setShown(await run());
      setRotating(null);
    }, failure);
  if (feeds === null) return null;
  return (
    <section className={SECTION}>
      <div className="flex items-center gap-2">
        <p className="flex-1 text-w-sm font-semibold text-w-strong">Feeds</p>
        {canChange && !making && (
          <button type="button" onClick={() => { setMaking(true); setShown(null); }} className={BTN_SECONDARY_SM}>
            <Plus size={12} weight="bold" /> New webhook
          </button>
        )}
      </div>
      <p className="text-w-xs text-w-muted">
        {agentName} wakes up when something new lands on these. Ask in chat to add one, like &quot;watch this RSS feed for critical CVEs&quot;. Every webhook call is checked before {agentName} sees it: who sent it, whether it is the kind of call you expect, and which fields are passed on.
      </p>
      {making && (
        <NewWebhookForm
          agentId={agentId}
          onCancel={() => setMaking(false)}
          onMade={(data) => {
            setMaking(false);
            setShown(data);
            refresh();
          }}
        />
      )}
      {shown && <FeedSecretCard data={shown} onDone={() => setShown(null)} />}
      <ul className="space-y-1.5">
        {feeds.length === 0 && (
          <li>
            <EmptyState icon={Rss} title="No feeds" description="Add an RSS feed or a webhook and they wake on new items." />
          </li>
        )}
        {feeds.map((f) => {
          const auth = feedAuthLine(f);
          return (
            <li key={f.id} {...(focusId === f.id ? SEARCH_ROW_FOCUS : {})} className={cn("flex items-start gap-2 rounded-md border border-w-line bg-w-field/70 px-2.5 py-1.5", f.paused && "opacity-60", SEARCH_ROW_RING)}>
              <span className="min-w-0 flex-1 space-y-0.5">
                <span className="flex items-center gap-1.5">
                  <span className="rounded-md border border-w-line2 px-1 text-w-3xs text-w-muted">{f.kind === "rss" ? "RSS" : "Webhook"}</span>
                  <span className="truncate font-medium">{f.name}</span>
                </span>
                <span className="block truncate font-mono text-w-xs text-w-text2 select-all" title={f.kind === "rss" ? (f.url ?? "") : (f.webhook_url ?? "")}>
                  {f.kind === "rss" ? f.url : f.webhook_url}
                </span>
                <span className="block text-w-xs text-w-muted">
                  {f.kind === "rss" ? feedEvery(f.poll_secs) : f.intake?.source ? `from ${f.intake.source}` : `${f.format} format`}
                  {f.filter ? ` · filter: ${f.filter}` : ""}
                  {` · ${f.item_count} delivered`}
                  {f.last_item_at ? <RelativeTime at={f.last_item_at} prefix=" · last " /> : ""}
                  {f.paused ? " · paused" : ""}
                </span>
                {auth && (
                  <span className={cn("flex items-center gap-1 text-w-xs", auth.tone === "ok" ? "text-w-ok" : auth.tone === "warn" ? "text-w-danger" : "text-w-muted")}>
                    {auth.tone === "ok" ? <ShieldCheck size={12} weight="bold" /> : <LockSimple size={12} weight="bold" />}
                    {auth.text}
                  </span>
                )}
                {f.kind === "webhook" && <FeedRanges agentId={agentId} feed={f} canChange={canChange} onSaved={refresh} />}
                {f.kind === "webhook" && f.checks && <span className="block text-w-xs text-w-muted">Checks: {f.checks}</span>}
                {f.kind === "webhook" && (f.turned_away ?? 0) > 0 && (
                  <span className="block text-w-xs text-w-warn">
                    {f.turned_away} {f.turned_away === 1 ? "call" : "calls"} turned away by its checks{f.turned_away_why ? `, last because ${f.turned_away_why}` : ""}
                  </span>
                )}
                {f.last_error && <span className="block text-w-xs text-w-danger">{f.last_error}</span>}
                {canChange && f.kind === "webhook" && (
                  <span className="flex flex-wrap gap-1.5 pt-1">
                    {f.secret_unseen && (
                      <button type="button" onClick={() => void show(() => worldApi.revealFeedSecret(agentId, f.id), "Could not show that secret.")} className={BTN_SECONDARY_SM}>
                        Show secret once
                      </button>
                    )}
                    {f.auth_mode === "address" ? (
                      <button type="button" onClick={() => void act(() => worldApi.updateFeedChecks(agentId, f.id, { auth: "secret" }), "Could not change how this sender is checked.")} className={BTN_SECONDARY_SM}>
                        Require the secret
                      </button>
                    ) : (
                      (f.allowed_ips?.length ?? 0) > 0 && (
                        <button type="button" onClick={() => void act(() => worldApi.updateFeedChecks(agentId, f.id, { auth: "address" }), "Could not change how this sender is checked.")} className={BTN_SECONDARY_SM}>
                          Accept by address only
                        </button>
                      )
                    )}
                    <button
                      type="button"
                      onClick={() => {
                        if (rotating !== f.id) {
                          setRotating(f.id);
                          return;
                        }
                        void show(() => worldApi.rotateFeedSecret(agentId, f.id), "Could not rotate that secret.");
                      }}
                      onBlur={() => setRotating(null)}
                      className={cn(BTN_SECONDARY_SM, rotating === f.id && "bg-w-alert text-w-onalert hover:bg-w-alert")}
                    >
                      {rotating === f.id ? "The old secret stops working. Rotate?" : f.has_secret ? "Rotate secret" : "Make secret"}
                    </button>
                  </span>
                )}
              </span>
              {canChange && (
                <>
                  <button
                    type="button"
                    onClick={() => void act(() => worldApi.pauseFeed(agentId, f.id, !f.paused), "Could not change that feed.")}
                    className={BTN_SECONDARY_SM}
                  >
                    {f.paused ? "Resume" : "Pause"}
                  </button>
                  <ConfirmDelete label={`Remove ${f.name}`} onConfirm={() => void act(() => worldApi.removeFeed(agentId, f.id), "Could not remove that feed.")} />
                </>
              )}
            </li>
          );
        })}
      </ul>
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </section>
  );
}


const AUTO_DELETE = [
  { value: "off", label: "Never delete" },
  { value: "7", label: "Delete after 7 days" },
  { value: "30", label: "Delete after 30 days" },
  { value: "90", label: "Delete after 90 days" },
];

export function ArchiveAgentRow({ agent, onUpdated }: { agent: WorldAgent; onUpdated: (a: WorldAgent) => void }) {
  const [after, setAfter] = useState("off");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (agent.is_main) return null;
  const days = after === "off" ? null : Number(after);
  const send = async (patch: { archived?: boolean; delete_after_days?: number | null }, failed: string) => {
    setBusy(true);
    setError(null);
    try {
      onUpdated(await worldApi.update(agent.id, patch));
    } catch {
      setError(failed);
    } finally {
      setBusy(false);
    }
  };
  if (agent.archived_at) {
    return (
      <div className="space-y-3 rounded-lg border-2 border-dashed border-w-line2 p-3">
        <p className="text-w-sm text-w-text2">
          {agent.name} is archived: it does no work and is hidden from the map and lists.{" "}
          {agent.archive_delete_at ? (
            <>
              It will be deleted for good <RelativeTime at={agent.archive_delete_at} />.
            </>
          ) : (
            "It stays until you restore or delete it."
          )}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" disabled={busy} onClick={() => void send({ archived: false }, "Could not restore them.")} className={BTN_PRIMARY}>
            <ArrowCounterClockwise size={14} weight="bold" />
            Restore
          </button>
          <WSelect
            ariaLabel="Auto delete"
            className="w-52"
            value={agent.archive_delete_at ? "keep" : "off"}
            onChange={(v) => v !== "keep" && void send({ delete_after_days: v === "off" ? null : Number(v) }, "Could not change auto delete.")}
            options={agent.archive_delete_at ? [{ value: "keep", label: "Keep the current date" }, ...AUTO_DELETE] : AUTO_DELETE}
            disabled={busy}
          />
        </div>
        {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
      </div>
    );
  }
  return (
    <div className="space-y-3 rounded-lg border-2 border-dashed border-w-line2 p-3">
      <p className="text-w-sm text-w-text2">Archive {agent.name} if you no longer need it but are not ready to delete it. It stops working and is hidden from the map and lists, and keeps its memory, chats and kit. Pause is for a short break.</p>
      <div className="flex flex-wrap items-center gap-2">
        <WSelect ariaLabel="Auto delete" className="w-52" value={after} onChange={setAfter} options={AUTO_DELETE} disabled={busy} />
        <button type="button" disabled={busy} onClick={() => void send({ archived: true, delete_after_days: days }, "Could not archive them.")} className={BTN_SECONDARY}>
          <Archive size={14} weight="bold" />
          Archive
        </button>
      </div>
      {error && <p role="alert" className="text-w-xs text-w-danger">{error}</p>}
    </div>
  );
}

export function RemoveAgentButton({ agent, onRemoved }: { agent: WorldAgent; onRemoved: () => void }) {
  const [armed, setArmed] = useState(false);
  const [failed, setFailed] = useState(false);
  if (agent.is_main) return null;
  return (
    <div className="flex items-center gap-3 rounded-lg border-2 border-dashed border-w-coral p-3">
      <span className="flex-1 text-w-sm text-w-text2">
        {failed ? "Could not remove them." : `Removing ${agent.name} deletes their memory, sessions and routines.`}
      </span>
      <button
        type="button"
        onClick={() => {
          if (!armed) {
            setArmed(true);
            return;
          }
          worldApi.remove(agent.id).then(onRemoved, () => setFailed(true));
        }}
        onBlur={() => setArmed(false)}
        className={cn(BTN, BTN_MD, armed ? "bg-w-alert text-w-onalert" : "bg-w-card hover:bg-w-sand")}
      >
        <Trash size={14} weight="bold" />
        {armed ? `Really remove ${agent.name}?` : "Remove from world"}
      </button>
    </div>
  );
}

const REPEATS = [
  { value: "0 9 * * *", label: "Every morning at 9" },
  { value: "0 9 * * 1-5", label: "Weekdays at 9" },
  { value: "0 * * * *", label: "Every hour" },
  { value: "0 18 * * 5", label: "Fridays at 6pm" },
  { value: "once", label: "Once, at a time" },
  { value: "custom", label: "Custom cron" },
];

function when(s: AgentSchedule): string {
  const preset = REPEATS.find((r) => r.value === s.cron_expression);
  if (preset) return preset.label;
  if (s.cron_expression) return `cron ${s.cron_expression}`;
  return s.run_at ? `Once, ${new Date(s.run_at).toLocaleString()}` : "Once";
}

const BEATS = [
  { value: "*/30 * * * *", label: "Every 30 minutes" },
  { value: "0 * * * *", label: "Every hour" },
  { value: "0 */3 * * *", label: "Every 3 hours" },
  { value: "0 9-18 * * 1-5", label: "Hourly, weekday work hours" },
];

function HeartbeatPanel({ agentId, agentName, beat, onChanged, focused }: { agentId: string; agentName: string; beat: AgentSchedule | undefined; onChanged: () => void; focused: boolean }) {
  const [checklist, setChecklist] = useState(beat?.prompt ?? "");
  const [every, setEvery] = useState(beat?.cron_expression ?? BEATS[1].value);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setChecklist(beat?.prompt ?? "");
    setEvery(beat?.cron_expression ?? BEATS[1].value);
  }, [beat?.id, beat?.prompt, beat?.cron_expression]);

  const options = BEATS.some((b) => b.value === every) ? BEATS : [...BEATS, { value: every, label: `cron ${every}` }];
  const dirty = !beat || checklist.trim() !== beat.prompt || every !== beat.cron_expression;

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      if (beat) await worldApi.updateSchedule(agentId, beat.id, { prompt: checklist.trim(), cron_expression: every });
      else await worldApi.addSchedule(agentId, { name: "Heartbeat", prompt: checklist.trim(), cron_expression: every, kind: "heartbeat" });
      onChanged();
    } catch {
      setError("Could not save the heartbeat.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div {...(focused ? SEARCH_ROW_FOCUS : {})} className={cn("space-y-2 rounded-md border border-w-line bg-w-field/70 px-2.5 py-2", beat && !beat.enabled && "opacity-70", SEARCH_ROW_RING)}>
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1">
          <span className="block font-medium">Heartbeat</span>
          <span className="block text-w-xs text-w-muted">
            {agentName} checks this list on a timer and only speaks up when something needs you.
            {beat?.enabled && beat.next_run_at ? ` Next ${new Date(beat.next_run_at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}.` : beat && !beat.enabled ? " Paused." : ""}
          </span>
        </span>
        {beat && (
          <>
            <button
              type="button"
              onClick={() => void worldApi.toggleSchedule(agentId, beat.id, !beat.enabled).then(onChanged)}
              className={BTN_SECONDARY_SM}
            >
              {beat.enabled ? "Pause" : "Resume"}
            </button>
            <ConfirmDelete label="Remove heartbeat" onConfirm={() => void worldApi.deleteSchedule(agentId, beat.id).then(onChanged)} />
          </>
        )}
      </div>
      <VoiceField
        multiline
        ariaLabel="Heartbeat checklist"
        placeholder={"- Any PR of mine with new review comments?\n- Anything failing in CI on main?\n- Slack messages that still need an answer?"}
        value={checklist}
        onChange={setChecklist}
      />
      <div className="flex items-center gap-2">
        <WSelect ariaLabel="Heartbeat interval" className="w-56" value={every} onChange={setEvery} options={options} />
        <button
          type="button"
          disabled={busy || !checklist.trim() || !dirty}
          onClick={() => void save()}
          className={BTN_PRIMARY}
        >
          {beat ? "Save" : "Start heartbeat"}
        </button>
        {error && <span className="text-w-sm text-w-danger">{error}</span>}
      </div>
    </div>
  );
}

const ACTION_TONES = { ok: "text-w-ok", warn: "text-w-warn", danger: "text-w-danger", muted: "text-w-muted" } as const;

function PrActions({ agentId, agentName, actions, focusId, onChanged }: { agentId: string; agentName: string; actions: AgentSchedule[]; focusId?: string; onChanged: () => void }) {
  const newestFirst = [...actions].reverse();
  return (
    <div className="space-y-1.5 border-t-2 border-dashed border-w-line pt-3">
      <p className="text-w-sm font-semibold text-w-strong">Pull request actions</p>
      <p className="text-w-xs text-w-muted">Merges, branch clean-ups, closes and check reruns {agentName} asked Loka to do without AI. Loka checks the rules again right before each one and skips it when anything changed.</p>
      <ul className="space-y-1.5">
        {newestFirst.map((s) => {
          const state = prActionState(s);
          const open = !s.outcome;
          return (
            <li key={s.id} {...(focusId === s.id ? SEARCH_ROW_FOCUS : {})} className={cn("flex items-start gap-2 rounded-md border border-w-line bg-w-field/70 px-2.5 py-1.5", SEARCH_ROW_RING)}>
              <span className="min-w-0 flex-1">
                <span className="block font-medium">{s.name}</span>
                <span className={cn("block text-w-xs", ACTION_TONES[state.tone])}>{state.text}</span>
                {s.prompt !== s.name && <span className="line-clamp-2 block text-w-xs text-w-text2">{s.prompt}</span>}
              </span>
              {s.task_id && (
                <button
                  type="button"
                  onClick={() => window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: s.task_id, agentId } }))}
                  className={BTN_SECONDARY_SM}
                >
                  Quest
                </button>
              )}
              <ConfirmDelete label={open ? `Cancel ${s.name}` : `Remove ${s.name}`} onConfirm={() => void worldApi.deleteSchedule(agentId, s.id).then(onChanged)} />
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export function SchedulesSection({ agentId, agentName, focusId }: { agentId: string; agentName: string; focusId?: string }) {
  const queryClient = useQueryClient();
  const list: AgentSchedule[] = useQuery({ queryKey: qk.agent.schedules(agentId), queryFn: () => worldApi.schedules(agentId) }).data ?? EMPTY_SCHEDULES;
  const [name, setName] = useState("");
  const [prompt, setPrompt] = useState("");
  const [repeat, setRepeat] = useState(REPEATS[0].value);
  const [cron, setCron] = useState("");
  const [at, setAt] = useState("");
  const [error, setError] = useState<string | null>(null);
  const refresh = () => void queryClient.invalidateQueries({ queryKey: qk.agent.schedules(agentId) });
  const reload = useCallback(() => void queryClient.invalidateQueries({ queryKey: qk.agent.schedules(agentId) }), [queryClient, agentId]);
  const ownScheduleChanged = useCallback((e: WorldEvent) => e.agent_id === agentId && (e.type.startsWith("action.") || e.type === "schedule.updated"), [agentId]);
  useWorldRefresh(reload, ownScheduleChanged);

  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await worldApi.addSchedule(agentId, {
        name: name.trim(),
        prompt: prompt.trim(),
        ...(repeat === "once"
          ? { run_at: new Date(at).toISOString() }
          : { cron_expression: repeat === "custom" ? cron.trim() : repeat }),
      });
      setName("");
      setPrompt("");
      refresh();
    } catch {
      setError("That schedule did not work. Check the time or cron.");
    }
  };

  const routines = list.filter((s) => s.kind !== "heartbeat" && s.kind !== "action");
  const actions = list.filter((s) => s.kind === "action");
  const beat = list.find((s) => s.kind === "heartbeat");
  const ready = name.trim() && prompt.trim() && (repeat === "once" ? Boolean(at) : repeat !== "custom" || cron.trim());

  return (
    <section className={SECTION}>
      <p className="text-w-sm font-semibold text-w-strong">Routines</p>
      <HeartbeatPanel agentId={agentId} agentName={agentName} beat={beat} onChanged={refresh} focused={Boolean(beat && beat.id === focusId)} />
      <p className="text-w-xs text-w-muted">{agentName} wakes up on these and does the prompt on their own.</p>
      <ul className="space-y-1.5">
        {routines.length === 0 && (
          <li>
            <EmptyState icon={CalendarBlank} title="No routines" description="Schedule recurring work and it runs on its own." />
          </li>
        )}
        {routines.map((s) => (
          <li key={s.id} {...(focusId === s.id ? SEARCH_ROW_FOCUS : {})} className={cn("flex items-start gap-2 rounded-md border border-w-line bg-w-field/70 px-2.5 py-1.5", !s.enabled && "opacity-60", SEARCH_ROW_RING)}>
            <span className="min-w-0 flex-1">
              <span className="block font-medium">{s.name}</span>
              <span className="block text-w-xs text-w-muted">
                {when(s)}
                {s.enabled && s.next_run_at ? <RelativeTime at={s.next_run_at} prefix=" · next " /> : s.enabled ? "" : " · paused"}
              </span>
              <span className="line-clamp-2 block text-w-xs text-w-text2">{s.prompt}</span>
            </span>
            <button
              type="button"
              onClick={() => void worldApi.toggleSchedule(agentId, s.id, !s.enabled).then(refresh)}
              className={BTN_SECONDARY_SM}
            >
              {s.enabled ? "Pause" : "Resume"}
            </button>
            <ConfirmDelete label={`Delete ${s.name}`} onConfirm={() => void worldApi.deleteSchedule(agentId, s.id).then(refresh)} />
          </li>
        ))}
      </ul>
      {actions.length > 0 && <PrActions agentId={agentId} agentName={agentName} actions={actions} focusId={focusId} onChanged={refresh} />}
      <form onSubmit={add} className="space-y-2 border-t-2 border-dashed border-w-line pt-3">
        <div className="flex gap-2">
          <input className={INPUT} placeholder="Morning standup" value={name} onChange={(e) => setName(e.target.value)} aria-label="Routine name" />
          <WSelect ariaLabel="Repeat" className="w-48" value={repeat} onChange={setRepeat} options={REPEATS} />
        </div>
        {repeat === "custom" && <input className={cn(INPUT, "w-full font-mono")} placeholder="*/30 * * * *" value={cron} onChange={(e) => setCron(e.target.value)} aria-label="Cron expression" />}
        {repeat === "once" && <input type="datetime-local" className={cn(INPUT, "w-full")} value={at} onChange={(e) => setAt(e.target.value)} aria-label="Run at" />}
        <VoiceField multiline ariaLabel="Routine prompt" placeholder="Check open PRs and tell me which need me." value={prompt} onChange={setPrompt} />
        <div className="flex items-center gap-2">
          <button type="submit" disabled={!ready} className={BTN_PRIMARY}>
            Add routine
          </button>
          {error && <span className="text-w-sm text-w-danger">{error}</span>}
        </div>
      </form>
    </section>
  );
}

function UsageBlock({ title, rows }: { title: string; rows: AgentUsageRow[] }) {
  if (rows.length === 0) return null;
  return (
    <div className="space-y-1">
      <p className="text-w-xs font-medium text-w-muted">{title}</p>
      <ul className="space-y-1">
        {rows.map((r) => {
          const tokens = r.input_tokens + r.cached_input_tokens + r.output_tokens;
          return (
            <li key={r.bucket} className="flex items-baseline gap-2 rounded-md border border-w-line bg-w-field/70 px-2.5 py-1.5 text-w-xs">
              <span className="min-w-0 flex-1 truncate">{r.bucket}</span>
              <span className="text-w-muted">{r.runs} runs</span>
              {tokens > 0 && <span className="text-w-muted">{compactNumber(tokens)} tokens</span>}
              {r.cost_usd > 0 && <span className="text-w-muted">cost ${r.cost_usd < 0.01 ? r.cost_usd.toFixed(4) : r.cost_usd.toFixed(2)}</span>}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export function UsageSection({ agent, canChange, onUpdated }: { agent: WorldAgent; canChange: boolean; onUpdated: (agent: WorldAgent) => void }) {
  const { id: agentId, name: agentName } = agent;
  const usageQuery = useQuery({ queryKey: qk.agent.usage(agentId), queryFn: () => worldApi.usage(agentId) });
  const usage: AgentUsage | undefined = usageQuery.data;
  const failed = !usage && usageQuery.isError;
  const [budget, setBudget] = useState(agent.daily_budget_usd ? String(agent.daily_budget_usd) : "");
  const [tokenBudget, setTokenBudget] = useState(agent.daily_token_budget ? tokenText(agent.daily_token_budget) : "");
  const [budgetError, setBudgetError] = useState("");
  const saveBudget = async (e: React.FormEvent) => {
    e.preventDefault();
    const value = budget.trim() === "" ? null : Number(budget);
    if (value !== null && (!Number.isFinite(value) || value < 0)) {
      setBudgetError("Enter a dollar amount, or leave it empty for no cap.");
      return;
    }
    const tokens = tokenBudget.trim() === "" ? null : parseTokens(tokenBudget);
    if (tokens !== null && Number.isNaN(tokens)) {
      setBudgetError("Enter tokens like 2000000, 500k or 2M, or leave it empty for no cap.");
      return;
    }
    setBudgetError("");
    try {
      onUpdated(
        await worldApi.update(agentId, {
          daily_budget_usd: value && value > 0 ? value : null,
          daily_token_budget: tokens && tokens > 0 ? tokens : null,
        }),
      );
    } catch {
      setBudgetError("Could not save the budget.");
    }
  };
  const saved = agent.daily_budget_usd ? String(agent.daily_budget_usd) : "";
  const savedTokens = agent.daily_token_budget ? tokenText(agent.daily_token_budget) : "";

  const totalTokens = usage ? usage.total.input_tokens + usage.total.cached_input_tokens + usage.total.output_tokens : 0;
  const totalLabel = usage ? formatUsage(usage.total.cost_usd, totalTokens) : "";

  return (
    <section className={SECTION}>
      <p className="text-w-sm font-semibold text-w-strong">Usage</p>
      <p className="text-w-xs text-w-muted">What {agentName} used in tokens, and what that costs at API list prices, last {usage?.days ?? 14} days.</p>
      {failed && <p className="text-w-sm text-w-danger">Could not load usage.</p>}
      {usage && usage.total.runs === 0 && <EmptyState icon={ChartBar} title="No runs in this window" description="Try a longer time range." />}
      {usage && usage.total.runs > 0 && (
        <>
          <p className="text-w-base">
            {usage.total.runs} runs{totalLabel ? ` · ${totalLabel}` : ""}
          </p>
          <UsageBlock title="By day" rows={usage.per_day} />
          <UsageBlock title="By model" rows={usage.per_backend} />
          <UsageBlock title="By work kind" rows={usage.per_kind} />
        </>
      )}
      {canChange && (
        <form onSubmit={saveBudget} className="space-y-1 border-t-2 border-dashed border-w-line pt-3">
          <div className="flex flex-wrap items-center gap-2">
            <label htmlFor={`budget-${agentId}`} className="text-w-sm text-w-text2">Daily budget (USD)</label>
            <input id={`budget-${agentId}`} className={cn(INPUT, "w-24")} inputMode="decimal" placeholder="No cap" value={budget} onChange={(e) => setBudget(e.target.value)} />
            <label htmlFor={`tokens-${agentId}`} className="text-w-sm text-w-text2">Tokens</label>
            <input id={`tokens-${agentId}`} className={cn(INPUT, "w-24")} placeholder="No cap" value={tokenBudget} onChange={(e) => setTokenBudget(e.target.value)} />
            <button
              type="submit"
              disabled={budget.trim() === saved && tokenBudget.trim() === savedTokens}
              className={BTN_PRIMARY}
            >
              Save
            </button>
          </div>
          <p className="text-w-xs text-w-muted">
            Every CLI counts. Dollars are API list prices: Claude, omp and OpenCode report them, Cursor and Codex are priced from public lists. With a Claude or ChatGPT plan you pay the plan, not these dollars, but the budget still counts them. Tokens are new input plus output, and also cover models with no known price. {agentName} warns you at 80%, and at 100% its running work stops and it pauses until 00:00 UTC.
          </p>
          {budgetError && <p className="text-w-sm text-w-danger">{budgetError}</p>}
        </form>
      )}
    </section>
  );
}

function describeServer(s: McpServer): string {
  const d = s.definition;
  if (typeof d.url === "string") return d.url;
  const args = Array.isArray(d.args) ? d.args.join(" ") : "";
  return `${typeof d.command === "string" ? d.command : ""} ${args}`.trim();
}

export function KitSection({ agentId, agentName, canInstall, focus }: { agentId: string; agentName: string; canInstall: boolean; focus?: { id: string; kind: "skill" | "mcp" } }) {
  const queryClient = useQueryClient();
  const kit: { mcp_servers: McpServer[]; skills: Skill[] } = useQuery({ queryKey: qk.agent.kit(agentId), queryFn: () => worldApi.kit(agentId) }).data ?? EMPTY_KIT;
  const servers: McpServer[] = useQuery({ queryKey: qk.mcpLibrary(), queryFn: () => worldApi.mcpLibrary() }).data ?? EMPTY_SERVERS;
  const skills: Skill[] = useQuery({ queryKey: qk.skillLibrary(), queryFn: () => worldApi.skillLibrary() }).data ?? EMPTY_SKILLS;
  const [mcpName, setMcpName] = useState("");
  const [mcpTarget, setMcpTarget] = useState("");
  const [skillUrl, setSkillUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const refresh = () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: qk.agent.kit(agentId) }),
      queryClient.invalidateQueries({ queryKey: qk.mcpLibrary() }),
      queryClient.invalidateQueries({ queryKey: qk.skillLibrary() }),
    ]);

  const hasServer = new Set(kit.mcp_servers.map((s) => s.id));
  const hasSkill = new Set(kit.skills.map((s) => s.id));
  const toggle = (kind: "mcp-servers" | "skills", id: string, on: boolean) =>
    worldApi
      .attach(agentId, kind, id, on)
      .then(refresh)
      .catch(() => setError("Could not change that."));

  const installMcp = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    const target = mcpTarget.trim();
    const parts = target.split(/\s+/);
    const definition = /^https?:\/\//.test(target) ? { url: target } : { command: parts[0], args: parts.slice(1) };
    try {
      await worldApi.installMcp(agentId, mcpName.trim(), definition);
      setMcpName("");
      setMcpTarget("");
      refresh();
    } catch {
      setError("Could not install that server.");
    }
  };

  const installSkill = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await worldApi.installSkill(agentId, { source_url: skillUrl.trim() });
      setSkillUrl("");
      refresh();
    } catch {
      setError("Could not fetch that skill. Use a raw SKILL.md link.");
    }
  };

  const row = (on: boolean, global: boolean, title: string, sub: string, onToggle: () => void, key: string, kind: "skill" | "mcp") => (
    <li key={key} {...(focus?.kind === kind && focus.id === key ? SEARCH_ROW_FOCUS : {})} className={cn("flex items-center gap-2 rounded-md border border-w-line bg-w-field/70 px-2.5 py-1.5", SEARCH_ROW_RING)}>
      <span className="min-w-0 flex-1">
        <span className="block font-medium">{title}</span>
        <span className="block truncate font-mono text-w-2xs text-w-muted">{sub}</span>
      </span>
      {global ? (
        <span className="text-w-2xs text-w-muted">Everyone has this</span>
      ) : (
        <button
          type="button"
          role="switch"
          aria-checked={on}
          aria-label={`${on ? "Remove" : "Give"} ${title}`}
          onClick={onToggle}
          className={cn(CHUNKY, "px-2 py-0.5 text-w-xs", on ? "bg-w-seafoam" : "bg-w-card hover:bg-w-sand")}
        >
          {on ? "Equipped" : "Equip"}
        </button>
      )}
    </li>
  );

  return (
    <section className={SECTION}>
      <p className="text-w-sm font-semibold text-w-strong">Kit</p>
      <p className="text-w-xs text-w-muted">Tools and skills {agentName} carries on every quest.</p>
      <p className={LABEL}>MCP servers</p>
      <ul className="space-y-1.5">
        {servers.length === 0 && (
          <li>
            <EmptyState icon={Plugs} title="No MCP servers" description="Install one to give this agent new tools." />
          </li>
        )}
        {servers.map((s) => row(hasServer.has(s.id), s.scope === "global", s.name, s.duplicate_of ? `Same as ${s.duplicate_of}, so agents get it once. Remove one.` : describeServer(s), () => void toggle("mcp-servers", s.id, !hasServer.has(s.id)), s.id, "mcp"))}
      </ul>
      <p className={LABEL}>Skills</p>
      <ul className="space-y-1.5">
        {skills.length === 0 && (
          <li>
            <EmptyState icon={BookOpen} title="No skills" description="Skills teach this agent how you like things done." />
          </li>
        )}
        {skills.map((s) => row(hasSkill.has(s.id), s.scope === "global", s.name, s.description || s.source_url || "", () => void toggle("skills", s.id, !hasSkill.has(s.id)), s.id, "skill"))}
      </ul>
      {canInstall && (
        <div className="space-y-2 border-t-2 border-dashed border-w-line pt-3">
          <form onSubmit={installMcp} className="flex gap-2">
            <input className={cn(INPUT, "max-w-[9rem]")} placeholder="github" value={mcpName} onChange={(e) => setMcpName(e.target.value)} aria-label="Server name" />
            <input className={cn(INPUT, "font-mono text-w-sm")} placeholder="npx -y @modelcontextprotocol/server-github or https://..." value={mcpTarget} onChange={(e) => setMcpTarget(e.target.value)} aria-label="Command or URL" />
            <button type="submit" disabled={!mcpName.trim() || !mcpTarget.trim()} className={BTN_PRIMARY}>
              Install
            </button>
          </form>
          <form onSubmit={installSkill} className="flex gap-2">
            <input className={cn(INPUT, "font-mono text-w-sm")} placeholder="https://raw.githubusercontent.com/.../SKILL.md" value={skillUrl} onChange={(e) => setSkillUrl(e.target.value)} aria-label="Skill URL" />
            <button type="submit" disabled={!skillUrl.trim()} className={BTN_PRIMARY}>
              Add skill
            </button>
          </form>
        </div>
      )}
      {error && <p className="text-w-sm text-w-danger">{error}</p>}
    </section>
  );
}
