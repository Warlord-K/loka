"use client";

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Desktop } from "@phosphor-icons/react";
import { qk } from "@/lib/query-keys";
import { worldApi, type GithubSettings, type WorldEvent } from "@/lib/world";
import { cn } from "@/lib/utils";
import { EmptyState, InfoNote, SettingsCard, SettingsRow, Switch } from "./settings-shell";
import { SetupHint, refreshReadiness, useFeature } from "./setup-hint";
import { RelativeTime } from "./relative-time";
import { PILL_OK, PILL_WARN } from "./styles";

const LABELS: Record<string, string> = {
  "github.review_requested": "Review requested",
  "github.mention": "Mentioned",
  "github.comment": "New comment",
  "github.ci": "CI",
  "github.assigned": "Assigned",
  "github.state_change": "State changed",
  "github.other": "GitHub",
};

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

export function isGithubEvent(type: string): boolean {
  return type.startsWith("github.");
}

export function githubLine(e: Pick<WorldEvent, "type" | "payload">): string {
  const p = (e.payload ?? {}) as Record<string, unknown>;
  const repo = text(p.repo_name) || text(p.repo);
  const target = typeof p.number === "number" ? `${repo}#${p.number}` : repo;
  const who = text(p.actor) ? ` from @${text(p.actor)}` : "";
  const snippet = text(p.snippet).replace(/\s+/g, " ").trim();
  const title = text(p.title);
  return `${LABELS[e.type] ?? "GitHub"} on ${target}${who}${title ? `: ${title}` : ""}${snippet ? ` - "${snippet.slice(0, 140)}${snippet.length > 140 ? "..." : ""}"` : ""}`;
}

export function githubLink(e: Pick<WorldEvent, "payload">): string | null {
  const url = text((e.payload ?? {}).url);
  return url.startsWith("https://github.com/") ? url : null;
}

export function GithubSection() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: qk.github(), queryFn: () => worldApi.github() }).data;
  const feature = useFeature("github_feed");
  const [saving, setSaving] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  if (!settings) return null;

  const toggle = async (enabled: boolean) => {
    setSaving(true);
    setNote(null);
    try {
      queryClient.setQueryData<GithubSettings>(qk.github(), await worldApi.setGithub(enabled));
      refreshReadiness();
    } catch {
      setNote("Could not save that. Try again.");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-3">
      <SettingsCard rows>
        <SettingsRow
          title="Read my GitHub notifications"
          description="Your Mac checks your GitHub notifications about once a minute with its gh login and hands new ones to your agents as events. Nothing is marked read, and the token never leaves your Mac."
          control={<Switch checked={settings.enabled} onChange={toggle} disabled={saving} label="Read my GitHub notifications" />}
        />
        <SettingsRow
          title="What agents get"
          description="Review requests, mentions, comments, CI results, assignments and state changes on threads you take part in. The notifications already waiting when you turn this on stay quiet. Agents hear about them only when they subscribe, and comments or CI on a quest's own pull request go straight to that quest."
        />
        {settings.last_item_at && (
          <SettingsRow title="Last delivery" control={<RelativeTime at={settings.last_item_at} className="text-w-xs text-w-muted" />} />
        )}
      </SettingsCard>
      {feature && settings.enabled && <SetupHint feature={feature} />}
      <div>
        <div className="mb-1 text-w-xs font-medium text-w-muted">Macs</div>
        {settings.devices.length === 0 ? (
          <EmptyState icon={Desktop} title="No Mac connected" description="Install the Loka desktop app on a Mac where gh is signed in." />
        ) : (
          <ul className="space-y-1.5">
            {settings.devices.map((d) => (
              <li key={d.id} className="flex items-center gap-2 text-w-sm">
                <span className="min-w-0 flex-1 truncate">
                  {d.name}
                  {d.login && <span className="text-w-muted"> as @{d.login}</span>}
                  {d.problem && <span className="block text-w-xs text-w-danger">{d.problem}</span>}
                </span>
                <span className={cn(d.online && d.available && !d.problem ? PILL_OK : PILL_WARN, "shrink-0")}>
                  {!d.online ? "Offline" : !d.available ? "gh not signed in" : d.problem ? "Needs a fix" : "Ready"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <InfoNote>When several Macs are online, the first one you connected does the reading. Tell your main agent things like &quot;watch PR owner/repo#12 for me&quot; to follow a single pull request.</InfoNote>
      {note && <p className="text-w-xs text-w-danger">{note}</p>}
    </div>
  );
}
