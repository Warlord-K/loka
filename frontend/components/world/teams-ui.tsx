"use client";

import { useMemo, useState, type FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, Check, Coins, Hash, PencilSimple, Plus, Star, Sword, Trash, UsersFour } from "@phosphor-icons/react";
import { SOLO } from "@/lib/edition";
import { qk } from "@/lib/query-keys";
import { cn } from "@/lib/utils";
import { worldApi, type Person, type WorldAgent } from "@/lib/world";
import { teamsApi, type Team, type TeamDraft, type TeamQuest } from "@/lib/teams";
import { WSelect } from "./controls";
import { WorldLinkButton } from "./message-links";
import { quoteProps } from "./quote";
import { BTN_DANGER, BTN_PRIMARY, BTN_SECONDARY, INPUT, ITEM_CARD, PILL_NEUTRAL, SURFACE } from "./styles";
import { Shimmer } from "./work-ui";
import { AgentFace, PersonAvatar } from "./channel-ui";
import { EmptyState, GHOST_ICON_BTN, InfoNote } from "./settings-shell";
import { StatusIcon, statusLabel } from "./quest-ui";
import { StarterTeamCard } from "./starter-team-ui";

const OPEN = new Set(["todo", "planning", "plan_review", "in_progress", "review"]);
const LIVE = new Set(["planning", "in_progress", "review"]);

function money(n: number): string {
  return n >= 0.01 ? `$${n.toFixed(2)}` : n > 0 ? "under $0.01" : "$0";
}

function canOpenTeam(team: Team, meId: string | null) {
  return team.owner_user_id === meId || team.members.some((member) => member.member_kind === "user" && member.member_id === meId);
}

function openChannel(id: string | null, loaded: boolean) {
  if (id) window.dispatchEvent(new CustomEvent("world:open-channel", { detail: { id, loaded } }));
}

function openQuest(taskId: string, agentId: string | null) {
  window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId, agentId, loaded: true } }));
}

function emptyDraft(): TeamDraft {
  return { name: "", purpose: "", rules: "", lead_agent_id: null, agent_ids: [], user_ids: [], channel: "" };
}

function personName(person: { name: string | null; email?: string | null }): string {
  return person.name || person.email?.split("@")[0] || "Someone";
}

function TeamEditor({
  agents,
  meId,
  team,
  onDone,
}: {
  agents: WorldAgent[];
  meId: string | null;
  team: Team | null;
  onDone: () => void;
}) {
  const [draft, setDraft] = useState<TeamDraft>(() =>
    team
      ? {
          name: team.name,
          purpose: team.purpose,
          rules: team.rules,
          lead_agent_id: team.lead_agent_id,
          agent_ids: team.members.filter((m) => m.member_kind === "agent").map((m) => m.member_id),
          user_ids: team.members.filter((m) => m.member_kind === "user").map((m) => m.member_id),
        }
      : emptyDraft(),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [person, setPerson] = useState("");
  const people: Person[] = useQuery({ queryKey: qk.people(), queryFn: () => worldApi.people(), enabled: !SOLO }).data ?? [];
  const selectedPeople = draft.user_ids.map((id) => {
    const found = people.find((entry) => entry.id === id);
    const member = team?.members.find((entry) => entry.member_kind === "user" && entry.member_id === id);
    return { id, name: found ? personName(found) : member?.name ?? "Someone" };
  });
  const personOptions = people
    .filter((entry) => entry.id !== meId && !draft.user_ids.includes(entry.id))
    .map((entry) => ({ value: entry.id, label: personName(entry), hint: entry.email ?? undefined }));
  const toggle = (id: string) =>
    setDraft((d) => {
      const on = d.agent_ids.includes(id);
      return {
        ...d,
        agent_ids: on ? d.agent_ids.filter((a) => a !== id) : [...d.agent_ids, id],
        lead_agent_id: on && d.lead_agent_id === id ? null : d.lead_agent_id,
      };
    });
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!draft.name.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (team) await teamsApi.update(team.id, draft, team);
      else await teamsApi.create({ ...draft, channel: draft.channel?.trim() || undefined });
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not save the team.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={submit} className={cn(SURFACE, "animate-[fade-in_180ms_ease-out] space-y-4 p-4 motion-reduce:animate-none sm:p-5")}>
      <div>
        <p className="text-w-md font-semibold text-w-strong">{team ? `Edit ${team.name}` : "New team"}</p>
        <p className="mt-0.5 text-w-xs text-w-muted">Pick who is on it and who leads. The lead splits quests and coordinates in the team channel.</p>
      </div>
      <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_14rem]">
        <input className={INPUT} placeholder="Name, like Research" value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} aria-label="Team name" />
        {!team && (
          <input className={INPUT} placeholder="#channel (optional)" value={draft.channel ?? ""} onChange={(e) => setDraft({ ...draft, channel: e.target.value })} aria-label="Team channel name" />
        )}
      </div>
      <textarea className={cn(INPUT, "block min-h-[4rem] w-full")} placeholder="What the team is for" value={draft.purpose} onChange={(e) => setDraft({ ...draft, purpose: e.target.value })} aria-label="Team purpose" />
      <textarea className={cn(INPUT, "block min-h-[3.5rem] w-full")} placeholder="Team rules, one per line (optional)" value={draft.rules} onChange={(e) => setDraft({ ...draft, rules: e.target.value })} aria-label="Team rules" />
      <div>
        <p className="mb-2 text-w-xs font-medium text-w-muted">Agents. Tap the star to pick the lead.</p>
        {agents.length === 0 ? (
          <p className="text-w-sm text-w-muted">You have no agents to put on a team yet.</p>
        ) : (
          <ul className="grid grid-cols-1 gap-2 @lg:grid-cols-2 @3xl:grid-cols-3">
            {agents.map((a) => {
              const on = draft.agent_ids.includes(a.id);
              const lead = draft.lead_agent_id === a.id;
              return (
                <li key={a.id} className={cn(ITEM_CARD, "flex items-center", on && "border-w-teal bg-w-aqua/60 hover:border-w-teal")}>
                  <button type="button" onClick={() => toggle(a.id)} aria-pressed={on} className="flex min-w-0 flex-1 items-center gap-2.5 px-3 py-2 text-left text-w-sm">
                    <span className={cn("grid h-5 w-5 shrink-0 place-items-center rounded-[6px] border-2 transition-colors", on ? "border-w-teal bg-w-teal text-w-strong" : "border-w-line2")}>
                      {on && <Check size={11} weight="bold" />}
                    </span>
                    <AgentFace agent={a} size={24} />
                    <span className="truncate font-medium text-w-strong">{a.name}</span>
                  </button>
                  {on && (
                    <button
                      type="button"
                      onClick={() => setDraft({ ...draft, lead_agent_id: lead ? null : a.id })}
                      aria-label={lead ? `${a.name} leads, tap to unset` : `Make ${a.name} the lead`}
                      title={lead ? "Lead" : "Make lead"}
                      className={cn(GHOST_ICON_BTN, "mr-1", lead ? "text-w-warn" : "text-w-faint")}
                    >
                      <Star size={16} weight={lead ? "fill" : "regular"} />
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
      {!SOLO && (
        <div>
          <p className="mb-2 text-w-xs font-medium text-w-muted">People</p>
          {selectedPeople.length > 0 && (
            <ul className="mb-2 flex flex-wrap gap-1.5">
              {selectedPeople.map((entry) => (
                <li key={entry.id} className={cn(PILL_NEUTRAL, "gap-1.5 py-1 pl-1 pr-1 text-w-xs font-medium")}>
                  <PersonAvatar id={entry.id} name={entry.name} size={18} soft />
                  <span>{entry.name}</span>
                  <button
                    type="button"
                    onClick={() => setDraft((current) => ({ ...current, user_ids: current.user_ids.filter((id) => id !== entry.id) }))}
                    aria-label={`Remove ${entry.name} from the team`}
                    className="rounded p-0.5 text-w-muted hover:bg-w-rose hover:text-w-danger"
                  >
                    <Trash size={12} weight="bold" />
                  </button>
                </li>
              ))}
            </ul>
          )}
          {personOptions.length > 0 && (
            <WSelect
              value={person}
              onChange={(id) => {
                setDraft((current) => ({ ...current, user_ids: [...current.user_ids, id] }));
                setPerson("");
              }}
              options={personOptions}
              placeholder="Add a person"
              ariaLabel="Person to add to the team"
              className="w-full sm:w-72"
            />
          )}
        </div>
      )}
      {error && <p role="alert" className="rounded-lg bg-w-rose px-3 py-2 text-w-sm text-w-danger">{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" onClick={onDone} className={BTN_SECONDARY}>
          Cancel
        </button>
        <button type="submit" disabled={busy || !draft.name.trim()} className={BTN_PRIMARY}>
          {busy ? <Shimmer>Saving</Shimmer> : team ? "Save" : "Form team"}
        </button>
      </div>
    </form>
  );
}

function QuestForm({ team, onDone }: { team: Team; onDone: () => void }) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!title.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await teamsApi.createQuest(team.id, title.trim(), description.trim() || title.trim());
      onDone();
    } catch {
      setError("Could not give the quest to the team. Does it have agents?");
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={submit} className="mt-3 animate-[fade-in_180ms_ease-out] space-y-2 border-t-2 border-dashed border-w-line pt-3 motion-reduce:animate-none">
      <input className={cn(INPUT, "w-full")} placeholder="Quest title" value={title} onChange={(e) => setTitle(e.target.value)} aria-label="Quest title" />
      <textarea className={cn(INPUT, "block min-h-[3.5rem] w-full")} placeholder="What done looks like" value={description} onChange={(e) => setDescription(e.target.value)} aria-label="Quest description" />
      {error && <p role="alert" className="text-w-sm text-w-danger">{error}</p>}
      <div className="flex justify-end gap-2">
        <button type="button" onClick={onDone} className={BTN_SECONDARY}>
          Cancel
        </button>
        <button type="submit" disabled={busy || !title.trim()} className={BTN_PRIMARY}>
          <Sword size={14} weight="bold" />
          {busy ? <Shimmer>Sending</Shimmer> : "Give to team"}
        </button>
      </div>
    </form>
  );
}

function TeamCard({
  team,
  byId,
  canEdit,
  canOpenChannel,
  onEdit,
}: {
  team: Team;
  byId: Map<string, WorldAgent>;
  canEdit: boolean;
  canOpenChannel: boolean;
  onEdit: () => void;
}) {
  const [questing, setQuesting] = useState(false);
  const [removing, setRemoving] = useState(false);
  const tops = team.quests.filter((q) => !q.parent_task_id || !team.quests.some((p) => p.id === q.parent_task_id));
  const partsOf = (id: string) => team.quests.filter((q) => q.parent_task_id === id);
  const open = tops.filter((q) => OPEN.has(q.status));
  const recent = tops.filter((q) => !OPEN.has(q.status)).slice(0, 3);
  const questRow = (q: TeamQuest, part: boolean) => (
    <li key={q.id}>
      <button type="button" onClick={() => openQuest(q.id, q.assigned_agent_id)} className={cn("flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-w-sand/70", part && "pl-7")}>
        <StatusIcon status={q.status} />
        <span className="min-w-0 flex-1 truncate text-w-text">
          {part && <span className="mr-1.5 text-w-faint">Part</span>}
          {LIVE.has(q.status) ? <Shimmer>{q.title}</Shimmer> : q.title}
        </span>
        <span className="shrink-0 text-w-2xs text-w-muted">
          {q.assigned_agent_id ? `${byId.get(q.assigned_agent_id)?.name ?? "Someone"} · ` : ""}
          {statusLabel(q.status)}
        </span>
      </button>
    </li>
  );
  const busy = team.members.filter((m) => m.member_kind === "agent" && (byId.get(m.member_id)?.jobs?.length ?? 0) > 0).length;
  return (
    <li {...quoteProps(`team:${team.id}`, team.name)} className={cn(ITEM_CARD, "flex flex-col p-4 select-text")}>
      <div className="flex items-start gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-w-aqua text-w-link">
          <UsersFour size={20} weight="duotone" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-w-md font-semibold text-w-strong">{team.name}</p>
          {team.purpose && <p className="mt-0.5 line-clamp-2 text-w-sm leading-relaxed text-w-muted">{team.purpose}</p>}
        </div>
        <WorldLinkButton kind="team" id={team.id} label={team.name} className={cn(GHOST_ICON_BTN, "shrink-0")} />
        {canEdit && (
          <div className="flex shrink-0 items-center gap-0.5">
            <button type="button" onClick={onEdit} aria-label={`Edit ${team.name}`} title="Edit" className={GHOST_ICON_BTN}>
              <PencilSimple size={16} />
            </button>
            <button
              type="button"
              onClick={() => {
                if (!removing) {
                  setRemoving(true);
                  return;
                }
                void teamsApi.remove(team.id);
              }}
              onBlur={() => setRemoving(false)}
              aria-label={removing ? `Confirm removing ${team.name}` : `Remove ${team.name}`}
              title={removing ? undefined : "Remove"}
              className={removing ? cn(BTN_DANGER, "h-8") : cn(GHOST_ICON_BTN, "hover:bg-w-rose hover:text-w-danger")}
            >
              <Trash size={16} />
              {removing && "Remove team and channel"}
            </button>
          </div>
        )}
      </div>
      <ul className="mt-3 flex flex-wrap gap-1.5">
        {team.members.map((m) => {
          const agent = m.member_kind === "agent" ? byId.get(m.member_id) : undefined;
          const lead = team.lead_agent_id === m.member_id;
          const working = (agent?.jobs?.length ?? 0) > 0;
          return (
            <li key={`${m.member_kind}:${m.member_id}`} className={cn(PILL_NEUTRAL, "gap-1.5 py-1 pl-1 pr-2 text-w-xs font-medium", lead && "border-w-gold/50 bg-w-lemon text-w-text")}>
              {agent ? <AgentFace agent={agent} size={18} /> : <PersonAvatar id={m.member_id} name={m.name} size={18} soft />}
              {working ? <Shimmer>{m.name}</Shimmer> : m.name}
              {lead && <Star size={11} weight="fill" className="text-w-warn" aria-label="Lead" />}
              {m.member_kind === "user" && <span className="font-normal text-w-muted">person</span>}
            </li>
          );
        })}
        {team.members.length === 0 && <li className="w-full"><EmptyState icon={UsersFour} title="No members yet" description="Add agents or people so the team has someone to route work to." /></li>}
      </ul>
      <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-w-xs text-w-muted">
        {team.channel_id && (
          <button type="button" onClick={() => openChannel(team.channel_id, canOpenChannel)} className="flex items-center gap-1 font-medium text-w-link transition-opacity hover:opacity-80">
            <Hash size={12} weight="bold" />
            {team.channel_name}
          </button>
        )}
        <span className="flex items-center gap-1">
          <Sword size={12} weight="bold" />
          {team.open_quests} open {team.open_quests === 1 ? "quest" : "quests"}
          {busy > 0 ? `, ${busy} at work` : ""}
        </span>
        <span className="flex items-center gap-1" title="Everything the team's agents spent, team quests or not">
          <Coins size={12} weight="bold" />
          {money(team.cost_today_usd)} today, {money(team.cost_week_usd)} this week
        </span>
      </div>
      {(open.length > 0 || recent.length > 0) && (
        <ul className="mt-3 space-y-0.5 border-t-2 border-dashed border-w-line pt-2 text-w-sm">
          {[...open, ...recent].flatMap((q) => [questRow(q, false), ...(OPEN.has(q.status) ? partsOf(q.id).map((p) => questRow(p, true)) : [])])}
        </ul>
      )}
      {canEdit &&
        (questing ? (
          <QuestForm team={team} onDone={() => setQuesting(false)} />
        ) : (
          <button type="button" onClick={() => setQuesting(true)} className={cn(BTN_SECONDARY, "mt-3 self-start")}>
            <Plus size={14} weight="bold" />
            Give the team a quest
          </button>
        ))}
    </li>
  );
}

export function TeamsView({
  teams,
  agents,
  meId,
  canWrite,
  focusId,
  onFocusChange,
}: {
  teams: Team[];
  agents: WorldAgent[];
  meId: string | null;
  canWrite: boolean;
  focusId?: string | null;
  onFocusChange?: (id: string | null) => void;
}) {
  const [editing, setEditing] = useState<Team | "new" | null>(null);
  const byId = useMemo(() => new Map(agents.map((a) => [a.id, a])), [agents]);
  const mine = useMemo(() => agents.filter((a) => a.owner_user_id === meId), [agents, meId]);
  const shownTeams = focusId ? teams.filter((team) => team.id === focusId && canOpenTeam(team, meId)) : teams;
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="w-full space-y-5 px-4 py-5 sm:px-5 xl:px-8">
        <div className="flex flex-wrap items-start justify-between gap-3">
          {focusId && (
            <button type="button" onClick={() => onFocusChange?.(null)} className={BTN_SECONDARY}>
              <ArrowLeft size={14} weight="bold" />
              All teams
            </button>
          )}
          <InfoNote className="max-w-3xl">
            Teams work together in their own channel. A quest given to a team goes to its lead, who splits it into parts and keeps everyone posted there, and it closes by itself once every part is done. You can also ask your main agent, for example: make a research team with Ada and Rook.
          </InfoNote>
          {canWrite && editing === null && (
            <button type="button" onClick={() => setEditing("new")} className={BTN_PRIMARY}>
              <Plus size={14} weight="bold" />
              New team
            </button>
          )}
        </div>
        {canWrite && editing === null && teams.length === 0 && <StarterTeamCard agentCount={agents.length} />}
        {editing === "new" && <TeamEditor agents={mine} meId={meId} team={null} onDone={() => setEditing(null)} />}
        {teams.length === 0 && editing === null ? (
          <EmptyState icon={UsersFour} title="No teams yet" description="Group agents into a team with a lead, a purpose and its own channel." />
        ) : (
          <ul className="grid items-start gap-3 @4xl:grid-cols-2">
            {shownTeams.map((t) =>
              editing !== null && editing !== "new" && editing.id === t.id ? (
                <li key={t.id} className="@4xl:col-span-2">
                  <TeamEditor agents={mine} meId={meId} team={editing} onDone={() => setEditing(null)} />
                </li>
              ) : (
                <TeamCard key={t.id} team={t} byId={byId} canEdit={canWrite && t.owner_user_id === meId} canOpenChannel={canOpenTeam(t, meId)} onEdit={() => setEditing(t)} />
              ),
            )}
          </ul>
        )}
      </div>
    </div>
  );
}
