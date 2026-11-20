"use client";

import { useEffect, useMemo, useState } from "react";
import {
  ArrowLeft,
  BookOpen,
  Notebook,
} from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { worldApi, type FeatureKey, type Skill, type WorldAgent } from "@/lib/world";
import { FeedsSection, KitSection, SchedulesSection, UsageSection } from "./agent-manage";
import { RichText, useAgentHandles } from "./chat-kit";
import { SetupHint, useFeature } from "./setup-hint";
import { BTN_GHOST, CHUNKY, PANEL } from "./styles";
import { WindowHeader, EmptyState } from "./settings-shell";
import { escapeTaken } from "@/lib/escape";

export type PlaceSheet =
  | { kind: "agents"; surface: "schedules" | "usage" | "feeds"; agentId?: string }
  | { kind: "setup"; feature: FeatureKey; title: string }
  | { kind: "skills"; agentId: string }
  | { kind: "notes"; agentId: string }
  | { kind: "notes-all" }
  | { kind: "kit"; agentId: string };

const AGENT_TITLE = { schedules: "Schedules and heartbeats", usage: "Usage and budgets", feeds: "Feeds" } as const;

export function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !escapeTaken(e)) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
}

export function Shell({ eyebrow, title, onClose, onBack, children }: { eyebrow: string; title: string; onClose: () => void; onBack?: () => void; children: React.ReactNode }) {
  useEscape(onClose);
  return (
    <div role="dialog" aria-modal="true" aria-label={title} className={cn(PANEL, "@container pointer-events-auto flex max-h-full w-full flex-col overflow-hidden")}>
      <WindowHeader
        title={title}
        subtitle={eyebrow && eyebrow !== "Loka" ? eyebrow : undefined}
        onClose={onClose}
        actions={
          onBack ? (
            <button type="button" onClick={onBack} className={cn(BTN_GHOST, "order-first")}>
              <ArrowLeft size={14} weight="bold" />
              Back
            </button>
          ) : undefined
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto p-4 sm:p-5">{children}</div>
    </div>
  );
}

function AgentPicker({ agents, value, onChange }: { agents: WorldAgent[]; value: string | null; onChange: (id: string) => void }) {
  return (
    <div role="tablist" aria-label="Agent" className="mb-4 flex flex-wrap gap-1.5">
      {agents.map((a) => (
        <button
          key={a.id}
          type="button"
          role="tab"
          aria-selected={a.id === value}
          onClick={() => onChange(a.id)}
          className={cn(CHUNKY, "px-2.5 py-1 text-w-sm", a.id === value ? "bg-w-invert text-w-oninvert" : "bg-w-card hover:bg-w-sand")}
        >
          {a.name}
        </button>
      ))}
    </div>
  );
}

function useKit(agentId: string) {
  const [kit, setKit] = useState<{ skills: Skill[]; failed: boolean } | null>(null);
  useEffect(() => {
    let live = true;
    setKit(null);
    worldApi
      .kit(agentId)
      .then((k) => live && setKit({ skills: k.skills, failed: false }))
      .catch(() => live && setKit({ skills: [], failed: true }));
    return () => {
      live = false;
    };
  }, [agentId]);
  return kit;
}

export function notesName(agent: WorldAgent): string {
  return `${agent.slug}-notes`;
}

function SkillBody({ skill, agents, onOpenAgent }: { skill: Skill; agents: WorldAgent[]; onOpenAgent: (id: string) => void }) {
  const handles = useAgentHandles(agents);
  const body = (skill.body ?? "").replace(/^---[\s\S]*?\n---\s*\n?/, "").trim();
  return (
    <div className="space-y-3">
      {skill.description && <p className="text-w-base text-w-text2">{skill.description}</p>}
      {body ? (
        <RichText text={body} handles={handles} onOpenAgent={onOpenAgent} className="select-text text-w-base leading-relaxed" />
      ) : (
        <p className="text-w-base text-w-muted">This page is blank.</p>
      )}
      {skill.source_url && (
        <a href={skill.source_url} target="_blank" rel="noopener noreferrer" className="inline-block text-w-sm text-w-link underline">
          Source
        </a>
      )}
    </div>
  );
}

function SkillsSheet({ agent, agents, onClose, onOpenAgent }: { agent: WorldAgent; agents: WorldAgent[]; onClose: () => void; onOpenAgent: (id: string) => void }) {
  const kit = useKit(agent.id);
  const [open, setOpen] = useState<string | null>(null);
  const skills = useMemo(() => (kit?.skills ?? []).filter((k) => k.name !== notesName(agent)), [kit, agent]);
  const current = skills.find((k) => k.id === open) ?? null;
  return (
    <Shell eyebrow={`${agent.name}'s bookshelf`} title={current ? current.name : "Skills"} onClose={onClose} onBack={current ? () => setOpen(null) : undefined}>
      {current ? (
        <SkillBody skill={current} agents={agents} onOpenAgent={onOpenAgent} />
      ) : !kit ? (
        <p className="text-w-base text-w-muted">Dusting off the shelf...</p>
      ) : kit.failed ? (
        <p className="text-w-base text-w-muted">The shelf could not be read right now.</p>
      ) : skills.length ? (
        <ul className="grid gap-2 sm:grid-cols-2">
          {skills.map((k) => (
            <li key={k.id}>
              <button type="button" onClick={() => setOpen(k.id)} className={cn(CHUNKY, "flex w-full items-start gap-2 bg-w-card p-3 text-left hover:bg-w-sand")}>
                <BookOpen size={18} weight="bold" className="mt-0.5 shrink-0 text-w-warn" />
                <span className="min-w-0">
                  <span className="block truncate font-semibold text-w-base">{k.name}</span>
                  {k.description && <span className="mt-0.5 line-clamp-2 block text-w-xs text-w-text2">{k.description}</span>}
                </span>
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-w-base text-w-muted">No skills on the shelf yet. Add them from {agent.name}&apos;s Settings, or at the machines here.</p>
      )}
    </Shell>
  );
}

function NotesSheet({ agent, agents, onClose, onBack, onOpenAgent }: { agent: WorldAgent; agents: WorldAgent[]; onClose: () => void; onBack?: () => void; onOpenAgent: (id: string) => void }) {
  const kit = useKit(agent.id);
  const notes = kit?.skills.find((k) => k.name === notesName(agent)) ?? null;
  return (
    <Shell eyebrow={`${agent.name}'s desk`} title="Notes" onClose={onClose} onBack={onBack}>
      {!kit ? (
        <p className="text-w-base text-w-muted">Finding the notebook...</p>
      ) : notes ? (
        <SkillBody skill={notes} agents={agents} onOpenAgent={onOpenAgent} />
      ) : (
        <p className="text-w-base text-w-muted">{agent.name} has not written any notes yet. Agents keep their notes up to date on their own.</p>
      )}
    </Shell>
  );
}

function AgentsSheet({
  surface,
  initialAgent,
  agents,
  meId,
  isAdmin,
  onClose,
  onUpdated,
}: {
  surface: "schedules" | "usage" | "feeds";
  initialAgent?: string;
  agents: WorldAgent[];
  meId: string | null;
  isAdmin: boolean;
  onClose: () => void;
  onUpdated: (agent: WorldAgent) => void;
}) {
  const mine = useMemo(() => agents.filter((a) => isAdmin || a.owner_user_id === meId), [agents, isAdmin, meId]);
  const [picked, setPicked] = useState<string | null>(initialAgent ?? null);
  const agent = mine.find((a) => a.id === picked) ?? mine[0] ?? null;
  const total = mine.reduce((n, a) => n + (a.cost_usd ?? 0), 0);
  const paused = mine.filter((a) => (a.paused_reason ?? "").startsWith("Daily budget of"));
  return (
    <Shell eyebrow={surface === "usage" ? "Market" : surface === "feeds" ? "Observatory" : "Clock Tower"} title={AGENT_TITLE[surface]} onClose={onClose}>
      {surface === "usage" && mine.length > 0 && (
        <div className="mb-4 rounded-lg border-2 border-w-line bg-w-field/50 p-3 text-w-sm">
          <p>
            Your agents have used <span className="font-semibold">${total.toFixed(2)}</span> in total at API list prices.
          </p>
          <p className="mt-1 text-w-xs text-w-muted">
            That is what the same tokens would cost on a pay as you go API key. When Claude Code or Codex is signed in with a plan (Claude Pro, Max, Team or Enterprise, or ChatGPT), the plan is what you pay, not this amount.
          </p>
          {paused.length > 0 && <p className="mt-1 text-w-warn">Paused by their daily budget: {paused.map((a) => a.name).join(", ")}.</p>}
        </div>
      )}
      {!agent ? (
        <p className="text-w-base text-w-muted">You have no agents of your own yet.</p>
      ) : (
        <>
          {mine.length > 1 && <AgentPicker agents={mine} value={agent.id} onChange={setPicked} />}
          {surface === "schedules" ? (
            <SchedulesSection key={agent.id} agentId={agent.id} agentName={agent.name} />
          ) : surface === "feeds" ? (
            <FeedsSection key={agent.id} agentId={agent.id} agentName={agent.name} canChange />
          ) : (
            <UsageSection key={agent.id} agent={agent} canChange onUpdated={onUpdated} />
          )}
        </>
      )}
    </Shell>
  );
}

function SetupSheet({ feature, title, onClose }: { feature: FeatureKey; title: string; onClose: () => void }) {
  const ready = useFeature(feature);
  return (
    <Shell eyebrow="Forge" title={title} onClose={onClose}>
      {ready ? <SetupHint feature={ready} onAction={onClose} /> : <p className="text-w-base text-w-muted">Checking what is set up...</p>}
    </Shell>
  );
}

export function PlaceSheetView({
  sheet,
  agents,
  meId,
  isAdmin,
  canInstall,
  onSheet,
  onClose,
  onOpenAgent,
  onAgentUpdated,
}: {
  sheet: PlaceSheet;
  agents: WorldAgent[];
  meId: string | null;
  isAdmin: boolean;
  canInstall: boolean;
  onSheet: (sheet: PlaceSheet) => void;
  onClose: () => void;
  onOpenAgent: (id: string) => void;
  onAgentUpdated: (agent: WorldAgent) => void;
}) {
  const agentOf = (id: string) => agents.find((a) => a.id === id) ?? null;
  if (sheet.kind === "agents") return <AgentsSheet key={`${sheet.surface}:${sheet.agentId ?? ""}`} surface={sheet.surface} initialAgent={sheet.agentId} agents={agents} meId={meId} isAdmin={isAdmin} onClose={onClose} onUpdated={onAgentUpdated} />;
  if (sheet.kind === "setup") return <SetupSheet feature={sheet.feature} title={sheet.title} onClose={onClose} />;
  if (sheet.kind === "notes-all") {
    const mine = agents.filter((a) => isAdmin || a.owner_user_id === meId);
    return (
      <Shell eyebrow="Library" title="Agents' notes" onClose={onClose}>
        {mine.length ? (
          <ul className="grid gap-2 sm:grid-cols-2">
            {mine.map((a) => (
              <li key={a.id}>
                <button type="button" onClick={() => onSheet({ kind: "notes", agentId: a.id })} className={cn(CHUNKY, "flex w-full items-center gap-2 bg-w-card p-3 text-left hover:bg-w-sand")}>
                  <BookOpen size={18} weight="bold" className="shrink-0 text-w-warn" />
                  <span className="truncate font-semibold text-w-base">{a.name}</span>
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <EmptyState icon={Notebook} title="No notes yet" description="Agents write notes here as they learn how you work." />
        )}
      </Shell>
    );
  }
  const agent = agentOf(sheet.agentId);
  if (!agent) {
    return (
      <Shell eyebrow="Loka" title="Nobody home" onClose={onClose}>
        <p className="text-w-base text-w-muted">This agent is not around any more.</p>
      </Shell>
    );
  }
  if (sheet.kind === "skills") return <SkillsSheet agent={agent} agents={agents} onClose={onClose} onOpenAgent={onOpenAgent} />;
  if (sheet.kind === "notes") return <NotesSheet agent={agent} agents={agents} onClose={onClose} onOpenAgent={onOpenAgent} />;
  return (
    <Shell eyebrow={`${agent.name}'s machines`} title="Kit" onClose={onClose}>
      <KitSection agentId={agent.id} agentName={agent.name} canInstall={canInstall} />
    </Shell>
  );
}
