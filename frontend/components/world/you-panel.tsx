"use client";

import { SOLO } from "@/lib/edition";
import { AgentAvatar, AgentFace } from "./channel-ui";
import { useCallback, useEffect, useMemo, useState } from "react";
import { ArrowsClockwise, Broom, Code, Crown, Desktop, GithubLogo, Star, Key, Keyboard, ListChecks, Microphone, Palette, Play, Plus, Scroll, ShareNetwork, SpeakerHigh, UserCircle } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { ShortcutList } from "./shortcuts";
import { worldApi, type WorldAgent, type WorldRule } from "@/lib/world";
import { portraitUrl } from "./characters";
import { ConfirmDelete, WSelect } from "./controls";
import { PLAYER_LOOK } from "./engine";
import type { Me } from "./hud";
import { BTN, BTN_MD, BTN_PRIMARY, BTN_SECONDARY, BTN_SM, INPUT, PILL_NEUTRAL, SEGMENT, SEGMENT_ITEM, SEGMENT_OFF, SEGMENT_ON } from "./styles";
import { MicCheck, PushToTalkKeys, VoiceField } from "./voice-ui";
import { Keycap } from "./keycap";
import { EnvironmentPanel } from "./environment";
import { CommandRulesPanel } from "./command-rules-ui";
import { SharedKeysPanel } from "./shared-keys";
import { SetupChecklist, SetupHint, useFeature } from "./setup-hint";
import { DevicesPanel } from "./desktop-devices";
import { GithubSection } from "./github-ui";
import { ApiTokensSection } from "./api-tokens";
import { StorageSection } from "./storage-ui";
import { useThemeMode, type ThemeMode } from "./theme";
import { previewSound, SOUND_KINDS, useSoundPrefs } from "./sounds";
import { UpdatesPage, useUpdates } from "./updates-ui";
import { GHOST_ICON_BTN, SettingsCard, SettingsGroup, SettingsRow, SettingsShell, Switch, type SettingsNavItem, EmptyState } from "./settings-shell";
import { runtimeName } from "@/lib/model-names";
import { escapeTaken } from "@/lib/escape";

const SCOPES = [
  { value: "all", label: "Always", hint: "talking and working" },
  { value: "talk", label: "When talking", hint: "how they reply to you" },
  { value: "work", label: "When working", hint: "how they do quests" },
];

const STATUS_PRESETS: [string, string][] = [
  ["📅", "In a meeting"],
  ["🎯", "Focusing"],
  ["🚌", "Commuting"],
  ["🍔", "Out for lunch"],
  ["🤒", "Out sick"],
  ["🌴", "On vacation"],
];

function StatusSection({ me, onStatus }: { me: Me | null; onStatus: (emoji: string, text: string) => Promise<boolean> }) {
  const [emoji, setEmoji] = useState(me?.status_emoji ?? "");
  const [text, setText] = useState(me?.status_text ?? "");
  const [note, setNote] = useState<string | null>(null);
  const current = Boolean(me?.status_emoji || me?.status_text);
  const changed = emoji.trim() !== (me?.status_emoji ?? "") || text.trim() !== (me?.status_text ?? "");
  const save = async (nextEmoji: string, nextText: string) => {
    const ok = await onStatus(nextEmoji.trim(), nextText.trim());
    setNote(ok ? (nextEmoji.trim() || nextText.trim() ? "Status set." : "Status cleared.") : "Keep it under 100 characters.");
    if (ok) {
      setEmoji(nextEmoji.trim());
      setText(nextText.trim());
    }
  };
  return (
    <SettingsGroup title="Status" description="Shown on your player card and to people who mention you.">
      <SettingsCard>
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            void save(emoji, text);
          }}
        >
          <div className="flex gap-2">
            <input className={cn(INPUT, "w-14 flex-none text-center")} value={emoji} maxLength={16} placeholder="💬" aria-label="Status emoji" onChange={(e) => setEmoji(e.target.value)} />
            <input className={INPUT} value={text} maxLength={100} placeholder="What's your status?" aria-label="Status text" onChange={(e) => setText(e.target.value)} />
          </div>
          <div className="flex flex-wrap gap-1.5">
            {STATUS_PRESETS.map(([e, t]) => (
              <button key={t} type="button" onClick={() => void save(e, t)} className="rounded-md border-2 border-w-line2 bg-w-field/60 px-2.5 py-1 text-w-xs transition-colors hover:border-w-ink hover:bg-w-paper">
                {e} {t}
              </button>
            ))}
          </div>
          <div className="flex items-center gap-2 border-t-2 border-dashed border-w-line pt-3">
            {note && <p role="status" className="min-w-0 flex-1 text-w-xs text-w-text2">{note}</p>}
            <span className="ml-auto flex gap-2">
              {current && (
                <button type="button" onClick={() => void save("", "")} className={BTN_SECONDARY}>
                  Clear
                </button>
              )}
              <button type="submit" disabled={!changed} className={BTN_PRIMARY}>
                Save status
              </button>
            </span>
          </div>
        </form>
      </SettingsCard>
    </SettingsGroup>
  );
}

const THEMES: { value: ThemeMode; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

function AppearancePage() {
  const [mode, setMode] = useThemeMode();
  return (
    <SettingsGroup title="Theme">
      <SettingsCard rows>
        <SettingsRow
          title="Color theme"
          description="System follows your computer's light or dark setting. Saved on this device."
          control={
            <div role="radiogroup" aria-label="Theme" className={SEGMENT}>
              {THEMES.map((t) => (
                <button key={t.value} type="button" role="radio" aria-checked={mode === t.value} onClick={() => setMode(t.value)} className={cn(SEGMENT_ITEM, mode === t.value ? SEGMENT_ON : SEGMENT_OFF)}>
                  {t.label}
                </button>
              ))}
            </div>
          }
        />
      </SettingsCard>
    </SettingsGroup>
  );
}

function SoundsPage() {
  const [prefs, setPrefs] = useSoundPrefs();
  return (
    <>
      <SettingsGroup title="Sound effects">
        <SettingsCard rows>
          <SettingsRow title="Play sounds for Loka events" description="Sounds stay quiet during a live voice call. Saved on this device." control={<Switch label="Play sounds for Loka events" checked={prefs.on} onChange={(on) => setPrefs({ ...prefs, on })} />} />
          <SettingsRow
            title="Volume"
            control={
              <div className={cn("flex w-56 items-center gap-3", !prefs.on && "opacity-50")}>
                <input
                  type="range"
                  min={0}
                  max={100}
                  step={5}
                  disabled={!prefs.on}
                  value={Math.round(prefs.volume * 100)}
                  onChange={(e) => setPrefs({ ...prefs, volume: Number(e.target.value) / 100 })}
                  onPointerUp={() => previewSound("notify")}
                  aria-label="Sound volume"
                  className="h-4 min-w-0 flex-1 accent-w-teal"
                />
                <span className="w-9 text-right font-mono text-w-xs tabular-nums text-w-muted">{Math.round(prefs.volume * 100)}%</span>
              </div>
            }
          />
        </SettingsCard>
      </SettingsGroup>
      <SettingsGroup title="Which sounds" description="Press play to hear one.">
        <SettingsCard rows className={cn(!prefs.on && "opacity-50")}>
          {SOUND_KINDS.map(({ kind, label, hint }) => (
            <SettingsRow
              key={kind}
              title={label}
              description={hint.charAt(0).toUpperCase() + hint.slice(1)}
              control={
                <>
                  <button type="button" disabled={!prefs.on} onClick={() => previewSound(kind)} aria-label={`Play the ${label.toLowerCase()} sound`} title="Play" className={GHOST_ICON_BTN}>
                    <Play size={14} weight="fill" />
                  </button>
                  <Switch label={label} disabled={!prefs.on} checked={prefs.kinds[kind]} onChange={(on) => setPrefs({ ...prefs, kinds: { ...prefs.kinds, [kind]: on } })} />
                </>
              }
            />
          ))}
        </SettingsCard>
      </SettingsGroup>
    </>
  );
}

function RuleRow({ rule, editable, onChanged }: { rule: WorldRule; editable: boolean; onChanged: () => void }) {
  const [body, setBody] = useState(rule.body);
  const dirty = body.trim() !== rule.body;
  const save = async (patch: { body?: string; scope?: string; enabled?: boolean }) => {
    await worldApi.updateRule(rule.id, patch).catch(() => {});
    onChanged();
  };
  return (
    <li className="space-y-3 px-4 py-3.5">
      <div className={cn(!rule.enabled && "opacity-55")}>
        {editable ? <VoiceField multiline value={body} onChange={setBody} ariaLabel="Rule" /> : <p className="text-w-sm leading-relaxed">{rule.body}</p>}
      </div>
      <div className="flex flex-wrap items-center gap-2.5">
        {editable ? (
          <WSelect size="sm" className="w-40" ariaLabel="When it applies" value={rule.scope} onChange={(v) => void save({ scope: v })} options={SCOPES} />
        ) : (
          <span className={PILL_NEUTRAL}>{SCOPES.find((x) => x.value === rule.scope)?.label}</span>
        )}
        {!editable && !rule.enabled && <span className={PILL_NEUTRAL}>Off</span>}
        <span className="ml-auto flex items-center gap-2.5">
          {dirty && (
            <button type="button" onClick={() => void save({ body: body.trim() })} className={cn(BTN, BTN_SM, "bg-w-teal font-semibold text-w-strong hover:bg-w-teal2")}>
              Save
            </button>
          )}
          {editable && <Switch label={rule.enabled ? "Turn this rule off" : "Turn this rule on"} checked={rule.enabled} onChange={(enabled) => void save({ enabled })} />}
          {editable && <ConfirmDelete label="Delete rule" onConfirm={() => void worldApi.deleteRule(rule.id).then(onChanged)} />}
        </span>
      </div>
    </li>
  );
}

const PAGE_KEY = "world.you.page";
const FOCUS_PAGES: Record<string, string> = {
  "world-devices": "devices",
  "world-rules": "rules",
  "world-environment": "keys",
  "world-shared-keys": "shared-keys",
  "world-tokens": "tokens",
  "world-github": "github",
};

function focusPage(id: string): string | null {
  return id.startsWith("page:") ? id.slice(5) : FOCUS_PAGES[id] ?? null;
}

function savedPage(): string | null {
  try {
    return window.localStorage.getItem(PAGE_KEY);
  } catch {
    return null;
  }
}

export function YouPanel({
  me,
  assistant,
  voiceReplies,
  onToggleVoice,
  onRename,
  onStatus,
  onOpenAssistant,
  onOpenPro,
  controlPlane,
  focusSection,
  onClose,
}: {
  me: Me | null;
  assistant: WorldAgent | null;
  voiceReplies: boolean;
  onToggleVoice: () => void;
  onRename: (name: string) => Promise<boolean>;
  onStatus: (emoji: string, text: string) => Promise<boolean>;
  onOpenAssistant: () => void;
  onOpenPro: () => void;
  controlPlane: "connecting" | "online" | "offline";
  focusSection: { id: string; at: number } | null;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !escapeTaken(e) && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const portrait = useMemo(() => portraitUrl(PLAYER_LOOK, 6), []);
  const [name, setName] = useState(me?.name ?? "");
  const [nameNote, setNameNote] = useState<string | null>(null);
  const [rules, setRules] = useState<WorldRule[]>([]);
  const [draft, setDraft] = useState("");
  const [scope, setScope] = useState("all");
  const [global, setGlobal] = useState(false);
  const admin = me?.role === "admin";
  const canWrite = me?.role === "write" || admin;
  const live = useFeature("live_voice");

  const updates = useUpdates();
  const items = useMemo<SettingsNavItem[]>(
    () =>
      [
        { id: "profile", label: "Profile", group: "Account", icon: UserCircle, keywords: "name handle email status" },
        canWrite && { id: "main-agent", label: "Main agent", group: "Account", icon: Star, keywords: "assistant juniper crown" },
        { id: "rules", label: "Rules", group: "Agents", icon: Scroll, keywords: "instructions talking working" },
        canWrite && { id: "setup", label: "Setup status", group: "Connections", icon: ListChecks, keywords: "ready readiness checklist" },
        canWrite && { id: "devices", label: "Devices", group: "Connections", icon: Desktop, keywords: "runner mac computer execution" },
        canWrite && me && { id: "keys", label: "Keys", group: "Connections", icon: Key, keywords: "environment provider api secrets values" },
        admin && !SOLO && { id: "shared-keys", label: "Shared keys", group: "Connections", icon: ShareNetwork, keywords: "team world admin" },
        canWrite && { id: "github", label: "GitHub", group: "Connections", icon: GithubLogo, keywords: "notifications pull requests reviews ci feed gh" },
        { id: "tokens", label: SOLO ? "Connect other tools" : "API tokens", group: "Connections", icon: Code, keywords: "mcp claude cursor api tokens" },
        { id: "pro", label: "Loka Pro", group: "Account", icon: Crown, keywords: "pro team organizations iphone ipad watch share waitlist" },
        canWrite && { id: "storage", label: "Storage", group: "Account", icon: Broom, keywords: "space size cleanup clean delete old disk usage tidy" },
        { id: "appearance", label: "Appearance", group: "Preferences", icon: Palette, keywords: "theme dark light" },
        { id: "sounds", label: "Sounds", group: "Preferences", icon: SpeakerHigh, keywords: "volume audio" },
        canWrite && { id: "voice", label: "Voice", group: "Preferences", icon: Microphone, keywords: "speech dictate speak" },
        { id: "shortcuts", label: "Shortcuts", group: "Preferences", icon: Keyboard, keywords: "keys keyboard hotkeys" },
        updates.view && { id: "updates", label: "Updates", group: "Preferences", icon: ArrowsClockwise, keywords: "version update upgrade automatic install restart" },
      ].filter(Boolean) as SettingsNavItem[],
    [canWrite, admin, me, updates.view],
  );
  const [page, setPage] = useState<string>(() => {
    const target = focusSection ? focusPage(focusSection.id) : null;
    return target ?? savedPage() ?? "profile";
  });
  useEffect(() => {
    const target = focusSection ? focusPage(focusSection.id) : null;
    if (target) setPage(target);
  }, [focusSection]);
  const active = page !== "pro" && items.some((i) => i.id === page) ? page : items[0].id;
  const select = (id: string) => {
    if (id === "pro") return onOpenPro();
    setPage(id);
    try {
      window.localStorage.setItem(PAGE_KEY, id);
    } catch {}
  };

  const refresh = useCallback(() => {
    worldApi.rules().then(setRules).catch(() => {});
  }, []);
  useEffect(() => refresh(), [refresh]);

  const mine = rules.filter((r) => r.user_id === me?.id);
  const everyone = rules.filter((r) => r.user_id === null);

  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!draft.trim()) return;
    await worldApi.addRule(draft.trim(), scope, global && admin).catch(() => {});
    setDraft("");
    refresh();
  };

  return (
    <SettingsShell
      title="You"
      items={items}
      active={active}
      onSelect={select}
      onClose={onClose}
      footer={
        <button
          type="button"
          onClick={() => select("profile")}
          className="flex w-full items-center gap-3 rounded-lg p-1.5 text-left transition-colors hover:bg-w-paper/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-w-teal"
        >
          <AgentAvatar portrait={portrait} size="2.5rem" tall />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-w-sm font-semibold text-w-strong">{me?.name || "You"}</span>
            <span className="block truncate text-w-xs text-w-muted">{me?.email ?? (me?.handle ? `@${me.handle}` : "")}</span>
          </span>
        </button>
      }
    >
      {active === "profile" && (
        <>
          <SettingsGroup title="Profile" description="How people and agents see you in Loka.">
            <SettingsCard rows>
              <div className="flex items-center gap-4 px-4 py-4">
                <AgentAvatar portrait={portrait} size="4.75rem" tall rounded="rounded-xl" />
                <div className="min-w-0">
                  <p className="truncate text-w-lg font-semibold leading-tight text-w-strong">{me?.name || "You"}</p>
                  <p className="mt-1.5 truncate text-w-sm text-w-muted">@{me?.handle}</p>
                </div>
              </div>
              <SettingsRow title="Display name" description={nameNote ?? "People and agents mention you with a handle made from it."}>
                <form
                  className="flex gap-2"
                  onSubmit={async (e) => {
                    e.preventDefault();
                    const ok = await onRename(name.trim());
                    setNameNote(ok ? "Saved. People and agents mention you with your new handle." : "Use letters or numbers, up to 40 characters.");
                  }}
                >
                  <input className={INPUT} value={name} maxLength={40} aria-label="Display name" onChange={(e) => setName(e.target.value)} />
                  <button type="submit" disabled={!name.trim() || name.trim() === me?.name} className={BTN_PRIMARY}>
                    Save
                  </button>
                </form>
              </SettingsRow>
              {me?.email && <SettingsRow title="Email" control={<span className="truncate text-w-sm text-w-text2">{me?.email ?? "None"}</span>} />}
            </SettingsCard>
          </SettingsGroup>
          <StatusSection me={me} onStatus={onStatus} />
        </>
      )}

      {active === "main-agent" && (
        <SettingsGroup title="Main agent" description="Your main agent hears about everything first and can reach your other agents.">
          <SettingsCard rows>
            {assistant ? (
              <SettingsRow
                title={
                  <span className="flex items-center gap-2.5">
                    <AgentFace agent={assistant} size={28} />
                    {assistant.name}
                  </span>
                }
                description={`Runs on ${runtimeName(assistant.cli, assistant.model)}`}
                control={
                  <button type="button" onClick={onOpenAssistant} className={BTN_SECONDARY}>
                    Open
                  </button>
                }
              />
            ) : (
              <SettingsRow
                title="No main agent yet"
                control={
                  <button type="button" onClick={onOpenAssistant} className={cn(BTN, BTN_MD, "bg-w-gold text-w-ink")}>
                    Summon your main agent
                  </button>
                }
              />
            )}
            <SettingsRow title="Change it" description="Any of your agents can be your main agent: use Make main agent in their Settings tab. Change their model, voice, persona or powers there too." />
          </SettingsCard>
        </SettingsGroup>
      )}

      {active === "rules" && (
        <>
          <SettingsGroup
            title="Rules"
            description="Instructions every one of your agents follows. When talking rules shape how they reply to you; when working rules apply to quests, code and reviews, and the reviewer checks them too. Changes reach agents on their next turn."
          >
            {canWrite && (
              <SettingsCard>
                <form onSubmit={add} className="space-y-3">
                  <VoiceField multiline placeholder="For example: always write tests for new code" ariaLabel="New rule" value={draft} onChange={setDraft} />
                  <div className="flex flex-wrap items-center gap-3">
                    <WSelect className="w-44" ariaLabel="When it applies" value={scope} onChange={setScope} options={SCOPES} />
                    {admin && (
                      <label className="flex items-center gap-2 text-w-sm">
                        <Switch label="For everyone" checked={global} onChange={setGlobal} />
                        For everyone
                      </label>
                    )}
                    <button type="submit" disabled={!draft.trim()} className={cn(BTN_PRIMARY, "ml-auto")}>
                      <Plus size={14} weight="bold" />
                      Add rule
                    </button>
                  </div>
                </form>
              </SettingsCard>
            )}
          </SettingsGroup>
          <SettingsGroup title={`Your rules (${mine.length})`}>
            {mine.length === 0 ? (
              <EmptyState icon={Scroll} title="No rules yet" description="Write one above. It reaches every agent on their next turn." />
            ) : (
              <SettingsCard rows>
                <ul className="divide-y-2 divide-w-line">
                  {mine.map((r) => (
                    <RuleRow key={`${r.id}:${r.body}:${r.scope}:${r.enabled}`} rule={r} editable={canWrite} onChanged={refresh} />
                  ))}
                </ul>
              </SettingsCard>
            )}
          </SettingsGroup>
          {everyone.length > 0 && (
            <SettingsGroup title={`Everyone's rules (${everyone.length})`} description={admin ? "Set by admins for every person. You can edit these." : "Set by admins for every person."}>
              <SettingsCard rows>
                <ul className="divide-y-2 divide-w-line">
                  {everyone.map((r) => (
                    <RuleRow key={`${r.id}:${r.body}:${r.scope}:${r.enabled}`} rule={r} editable={admin} onChanged={refresh} />
                  ))}
                </ul>
              </SettingsCard>
            </SettingsGroup>
          )}
        </>
      )}

      {active === "setup" && (
        <SettingsGroup title="Setup status" description="What works right now, and the next step for anything that needs setup.">
          <SetupChecklist onAction={() => select("devices")} />
        </SettingsGroup>
      )}

      {active === "devices" && (
        <SettingsGroup title="Devices" description="Agents use these computers for repositories and coding tools.">
          <SettingsCard>
            <DevicesPanel controlPlane={controlPlane} embedded />
          </SettingsCard>
        </SettingsGroup>
      )}

      {active === "keys" && me && (
        <SettingsGroup title="Keys" description="Store a value outside chat, then choose the agents that may use it while working. Values are never shown again.">
          <EnvironmentPanel ownerId={me.id} embedded />
        </SettingsGroup>
      )}

      {active === "keys" && me && (
        <SettingsGroup title="Command rules" description="Limit which shell commands your agents may run while working.">
          <CommandRulesPanel ownerId={me.id} />
        </SettingsGroup>
      )}

      {active === "shared-keys" && (
        <SettingsGroup title="Shared keys" description="Keys admins set for team agents, like a public bot that needs a Linear or GitHub token. Nobody can read a value back, not even admins. An agent's own key with the same name wins.">
          <SharedKeysPanel embedded />
        </SettingsGroup>
      )}

      {active === "github" && (
        <SettingsGroup
          title="GitHub"
          description="Let agents hear about your GitHub notifications as they happen, instead of checking on a timer."
          action={
            <span className="grid h-10 w-10 place-items-center rounded-xl border-2 border-w-line bg-w-card">
              <GithubLogo size={22} weight="duotone" className="text-w-text" />
            </span>
          }
        >
          <GithubSection />
        </SettingsGroup>
      )}

      {active === "tokens" && (
        <SettingsGroup title={SOLO ? "Connect other tools" : "API tokens"} description="Connect Claude Code, Cursor or any MCP client to your agents.">
          <SettingsCard>
            <ApiTokensSection canWrite={canWrite} />
          </SettingsCard>
        </SettingsGroup>
      )}

      {active === "storage" && canWrite && <StorageSection />}
      {active === "appearance" && <AppearancePage />}
      {active === "sounds" && <SoundsPage />}
      {active === "updates" && <UpdatesPage state={updates} autoNote="Updates to only the server or the web app apply in place, with no restart." />}

      {active === "voice" && (
        <SettingsGroup title="Voice">
          <SettingsCard rows>
            <SettingsRow
              title="Speak replies to dictated messages"
              description="Speech runs in your browser (Moonshine to listen, Kokoro to speak), so it is free and private. Each agent's voice is set in their Settings tab."
              control={<Switch label="Speak replies to dictated messages" checked={voiceReplies} onChange={() => onToggleVoice()} />}
            />
            <SettingsRow
              title="Talk into any message box"
              description={
                <>
                  Tap or hold the microphone in a message box, or hold <PushToTalkKeys /> while typing. Away from a box, hold <Keycap size="sm">V</Keycap>. Agents see the message is spoken and answer in a few short sentences.
                </>
              }
              control={<MicCheck />}
            />
          </SettingsCard>
          {live && live.state !== "ready" && live.state !== "unavailable" && <SetupHint feature={live} compact className="mt-3" />}
        </SettingsGroup>
      )}

      {active === "shortcuts" && (
        <SettingsGroup title="Keyboard shortcuts" description="Letter keys work when you are not typing in a field.">
          <SettingsCard>
            <ShortcutList />
          </SettingsCard>
        </SettingsGroup>
      )}
    </SettingsShell>
  );
}
