"use client";

import { useEffect, useState } from "react";
import { CheckCircle, Code, MagnifyingGlass, Robot, UsersFour, type Icon as PhosphorIcon } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { starterTeamApi, type StarterJob, type StarterPlan } from "@/lib/starter-team";
import { TEAMS_CHANGED } from "@/lib/teams";
import { BTN_PRIMARY, BTN_SECONDARY, BTN_SECONDARY_SM, ITEM_CARD, PILL_NEUTRAL, SURFACE } from "./styles";
import { InfoNote, Switch } from "./settings-shell";
import { runtimeName } from "@/lib/model-names";
import { Shimmer } from "./work-ui";

const HIDDEN_KEY = "world.starterTeam.hidden";

const JOB_ICON: Record<StarterJob, PhosphorIcon> = { coder: Code, reviewer: MagnifyingGlass };

function hiddenBefore(): boolean {
  try {
    return window.localStorage.getItem(HIDDEN_KEY) === "1";
  } catch {
    return false;
  }
}

function hideForNow() {
  try {
    window.localStorage.setItem(HIDDEN_KEY, "1");
  } catch {}
}

function showAgain() {
  try {
    window.localStorage.removeItem(HIDDEN_KEY);
  } catch {}
}

export function StarterTeamCard({ agentCount, className }: { agentCount: number; className?: string }) {
  const [plan, setPlan] = useState<StarterPlan | null>(null);
  const [hidden, setHidden] = useState(true);
  const [team, setTeam] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  useEffect(() => {
    setHidden(hiddenBefore());
  }, []);

  useEffect(() => {
    let live = true;
    starterTeamApi
      .plan()
      .then((p) => live && setPlan(p))
      .catch(() => live && setPlan(null));
    return () => {
      live = false;
    };
  }, [agentCount]);

  const build = async () => {
    setBusy(true);
    setError(null);
    try {
      const built = await starterTeamApi.build(team);
      setDone(built.summary);
      setPlan(built);
      if (built.team) window.dispatchEvent(new CustomEvent(TEAMS_CHANGED));
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : "Could not set up the starter team");
    } finally {
      setBusy(false);
    }
  };

  if (done) {
    return (
      <div className={cn(SURFACE, "animate-[fade-in_180ms_ease-out] p-4 text-w-sm motion-reduce:animate-none", className)}>
        <p className="flex items-start gap-2 whitespace-pre-line leading-relaxed text-w-text">
          <CheckCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-w-ok" />
          {done}
        </p>
        <div className="mt-3 flex justify-end">
          <button type="button" onClick={() => setDone(null)} className={BTN_SECONDARY}>
            Got it
          </button>
        </div>
      </div>
    );
  }
  if (!plan) return null;
  const missing = plan.seats.filter((s) => !s.exists);
  if (plan.complete || missing.length === 0) return null;
  if (hidden) {
    return (
      <div className={cn("flex items-center gap-3 rounded-xl border-2 border-dashed border-w-line2 px-4 py-2.5 text-w-sm text-w-text2", className)}>
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-w-aqua text-w-link">
          <UsersFour size={16} weight="duotone" />
        </span>
        <span className="min-w-0 flex-1">Want a ready-made starter team?</span>
        <button
          type="button"
          onClick={() => {
            showAgain();
            setHidden(false);
          }}
          className={BTN_SECONDARY_SM}
        >
          Show
        </button>
      </div>
    );
  }
  const coders = plan.seats.filter((s) => s.job === "coder").length;
  const first = !plan.seats.some((s) => s.exists);

  return (
    <div className={cn(ITEM_CARD, "animate-[fade-in_180ms_ease-out] p-4 motion-reduce:animate-none sm:p-5", className)}>
      <div className="flex items-start gap-3">
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-w-aqua text-w-link">
          <UsersFour size={20} weight="duotone" />
        </span>
        <div className="min-w-0">
          <p className="text-w-md font-semibold text-w-strong">{first ? "Starter team" : "Finish your starter team"}</p>
          <p className="mt-0.5 text-w-sm leading-relaxed text-w-muted">
            {first
              ? `A well-run World in one click. ${plan.coordinator ?? "Your main agent"} stays your coordinator and these move in:`
              : "Some of your starter team is missing. These would move in:"}
          </p>
        </div>
      </div>
      <ul className="mt-4 grid gap-2 @2xl:grid-cols-2">
        {missing.map((s) => {
          const JobIcon = JOB_ICON[s.job] ?? Robot;
          return (
            <li key={s.seat} className="flex items-start gap-3 rounded-lg bg-w-sand/50 px-3 py-2.5 text-w-sm">
              <span className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-w-paper text-w-link ring-1 ring-inset ring-w-line2">
                <JobIcon size={16} weight="duotone" />
              </span>
              <span className="min-w-0 flex-1">
                <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <span className="font-semibold text-w-strong">{s.name}</span>
                  <span className="text-w-muted">{s.job_label}</span>
                  <span className={PILL_NEUTRAL}>{runtimeName(s.cli, s.model)}</span>
                </span>
                <span className="mt-0.5 block text-w-xs leading-relaxed text-w-muted">{s.duty}</span>
              </span>
            </li>
          );
        })}
      </ul>
      <InfoNote className="mt-3">
        Everyone gets their own notes and a nightly notes and reset, and a rule names each agent&apos;s one job. Agents follow the pull requests, Slack channels and threads that matter themselves, so nothing sweeps on a timer. Remove any of them later like any other agent.
      </InfoNote>
      {coders >= 2 && !plan.team && (
        <div className="mt-3 flex items-center justify-between gap-3 rounded-lg border-2 border-w-line px-3 py-2.5 text-w-sm">
          <span className="text-w-text">Group the coders in a team with its own channel</span>
          <Switch checked={team} disabled={busy} onChange={setTeam} label="Group the coders in a team" />
        </div>
      )}
      {error && <p role="alert" className="mt-3 rounded-lg bg-w-rose px-3 py-2 text-w-sm text-w-danger">{error}</p>}
      <div className="mt-4 flex flex-wrap items-center justify-end gap-2">
        {!busy && (
          <button
            type="button"
            onClick={() => {
              hideForNow();
              setHidden(true);
            }}
            className={BTN_SECONDARY}
          >
            Not now
          </button>
        )}
        <button type="button" onClick={() => void build()} disabled={busy} className={BTN_PRIMARY}>
          {busy ? <Shimmer>Moving in</Shimmer> : first ? "Set up starter team" : `Add ${missing.length === 1 ? missing[0].name : `${missing.length} agents`}`}
        </button>
      </div>
    </div>
  );
}
