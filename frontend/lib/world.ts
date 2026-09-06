import type { Accessory, Trade, LokaLook, Resident, LokaShopStyle } from "@/components/world/types";
import { hash, lookFor, pick, text } from "./looks";

export { lookFor };

export type BackendModels = { models: string[]; default_model: string; labels?: Record<string, string>; default?: boolean };

export const RUN_PAGE = 60;

export type ApprovalMode = "auto" | "full" | "read_only";

export const APPROVAL_MODES: { value: ApprovalMode; label: string; hint: string }[] = [
  { value: "auto", label: "Auto approve", hint: "A safety check approves each action" },
  { value: "full", label: "Full access", hint: "Skips every permission check" },
  { value: "read_only", label: "Read only", hint: "Can look, cannot change anything" },
];

export type ReviewerChoice = { cli?: string; model: string };

export const CLI_OPTIONS: { value: string; label: string }[] = [
  { value: "claude", label: "Claude Code" },
  { value: "codex", label: "Codex" },
  { value: "cursor", label: "Cursor" },
  { value: "omp", label: "oh-my-pi" },
];

export const OPENCODE_OFF_NOTE = "OpenCode is off by default. A device admin can enable it.";
export const OPENCODE_DISABLED_NOTE = "OpenCode is disabled on this device. Switch to another CLI to keep this agent working.";

export function cliOptions(opencodeEnabled: boolean, current?: string): { value: string; label: string }[] {
  if (opencodeEnabled) return [...CLI_OPTIONS, { value: "opencode", label: "OpenCode" }];
  if (current === "opencode") return [...CLI_OPTIONS, { value: "opencode", label: "OpenCode (disabled)" }];
  return CLI_OPTIONS;
}

export function runnerHasOpencode(runner: Pick<RunnerView, "capabilities"> | null | undefined) {
  const backends = runner?.capabilities?.backends as Record<string, unknown> | undefined;
  return backends?.opencode === true;
}

export function reviewerLabel(choice: ReviewerChoice | null | undefined, agent: Pick<WorldAgent, "cli" | "model">) {
  if (!choice?.model) return `${agent.cli} ${agent.model}`;
  return `${choice.cli ?? agent.cli} ${choice.model}`;
}

export interface WorldAgent {
  id: string;
  slug: string;
  name: string;
  avatar_emoji: string;
  cli: string;
  model: string;
  runner_id: string | null;
  runner_name: string | null;
  pending_switch?: { cli: string; model: string } | null;
  persona: string | null;
  look: Record<string, unknown>;
  gear: Record<string, unknown>;
  status: "idle" | "thinking" | "away" | "waiting" | "error";
  status_detail: string | null;
  visibility: string;
  owner_user_id: string | null;
  tags: string[];
  expertise: string | null;
  revoked_powers: string[];
  approval_mode?: ApprovalMode;
  reviewer?: ReviewerChoice | null;
  paused_reason?: string | null;
  archived_at?: string | null;
  archive_delete_at?: string | null;
  daily_budget_usd?: number | null;
  daily_token_budget?: number | null;
  is_main: boolean;
  cost_usd: number;
  tokens: number;
  jobs?: AgentJob[];
}

export interface AgentJob {
  run_id: string;
  session_id?: string | null;
  task_id: string | null;
  kind: "task" | "review" | "followup" | "publish";
  status: "queued" | "running";
  title: string | null;
  activity: string | null;
  started_at: string;
}

export const TRADES: { id: Trade; label: string; verb: string }[] = [
  { id: "carpenter", label: "Carpenter", verb: "At the workbench" },
  { id: "smith", label: "Smith", verb: "At the forge" },
  { id: "alchemist", label: "Alchemist", verb: "Brewing" },
  { id: "wizard", label: "Wizard", verb: "Casting" },
  { id: "warrior", label: "Warrior", verb: "Battling" },
];

const TRADE_WORDS: [RegExp, Trade][] = [
  [/warrior|battle|fight|guard|knight|test|qa\b|bug|secur|hunt/, "warrior"],
  [/wizard|spell|magic|write|writer|doc|scribe|book|copy|design|prompt/, "wizard"],
  [/alchem|potion|brew|data|research|analy|ml\b|model|science|chem/, "alchemist"],
  [/smith|forge|metal|infra|deploy|devops|rust|backend|server|ops\b|database/, "smith"],
  [/carpent|wood|build|craft|frontend|\bui\b|ios|app|maker/, "carpenter"],
];

export function tradeFor(agent: Pick<WorldAgent, "slug" | "look" | "tags" | "persona" | "expertise">): Trade {
  const chosen = typeof agent.look?.trade === "string" ? agent.look.trade.toLowerCase() : "";
  const direct = TRADES.find((t) => t.id === chosen);
  if (direct) return direct.id;
  const words = [chosen, ...(agent.tags ?? []), agent.expertise ?? "", agent.persona ?? ""].join(" ").toLowerCase();
  const found = TRADE_WORDS.find(([re]) => re.test(words));
  if (found) return found[1];
  return pick(TRADES, hash(agent.slug), 7).id;
}

export const SUBSCRIPTION_EVENTS: { id: string; label: string; detail: string }[] = [
  { id: "slack.notification", label: "Slack notifications", detail: "DMs, mentions, highlights and thread replies, grouped into one turn per burst" },
  { id: "github.", label: "GitHub notifications", detail: "Review requests, mentions, comments and CI from your GitHub, needs the GitHub feed in You" },
  { id: "task.finished", label: "Quest finished", detail: "Any of your agents finished a quest" },
  { id: "run.failed", label: "Run failed", detail: "A task, review or follow-up run failed" },
  { id: "run.timed_out", label: "Run timed out", detail: "A run hit its time limit" },
  { id: "task.merged", label: "PR merged", detail: "A quest's pull request was merged" },
  { id: "task.pr_closed", label: "PR closed", detail: "A quest's pull request was closed unmerged" },
  { id: "task.assigned", label: "Quest assigned", detail: "Someone handed an agent a quest" },
  { id: "review.finished", label: "Review finished", detail: "An independent review came back" },
  { id: "attention.requested", label: "Needs a person", detail: "An agent asked a person to decide something" },
  { id: "agent.budget", label: "Budget", detail: "An agent used most or all of its daily budget" },
  { id: "agent.created", label: "New agent", detail: "A new agent joined your world" },
  { id: "channel.message", label: "Channel posts", detail: "New posts in public channels or ones it is in" },
  { id: "timer.done", label: "Timers", detail: "A timer went off" },
  { id: "feed.item", label: "Feed items", detail: "New items on a feed another of your agents watches" },
];

export interface GithubDevice {
  id: string;
  name: string;
  online: boolean;
  available: boolean;
  login: string | null;
  problem: string | null;
  checked_at: string | null;
}

export interface GithubSettings {
  enabled: boolean;
  primed_at: string | null;
  last_item_at: string | null;
  devices: GithubDevice[];
}

export interface AgentWatch {
  id: string;
  event_type: string;
  match: Record<string, string | number | boolean>;
  task_id: string | null;
  expires_at: string | null;
  note: string | null;
  created_at: string;
}

export const POWERS: { id: string; label: string; detail: string }[] = [
  { id: "manage_agents", label: "Manage agents", detail: "Create agents and change their model, CLI, persona and look" },
  { id: "assign_others", label: "Assign quests to others", detail: "Create quests for other agents, not just itself" },
  { id: "schedules", label: "Routines", detail: "Set up routines that wake an agent on a schedule" },
  { id: "install", label: "Install", detail: "Install skills and MCP servers" },
  { id: "oversee", label: "Oversee", detail: "Follow up on and review other agents' sessions, see key grants" },
  { id: "manage_quests", label: "Cancel and delete quests", detail: "Cancel or delete stuck quests and stop their work; others' quests also need Oversee" },
  { id: "feeds", label: "Feeds", detail: "Watch RSS feeds and receive webhook alerts that wake it up" },
  { id: "slack", label: "Slack", detail: "Post and reply in Slack through the Slack app on your Mac" },
  { id: "govern", label: "Pause and budgets", detail: "Pause and resume agents and set their daily budgets and approval modes, when you ask" },
  { id: "manage_powers", label: "Manage powers", detail: "Grant or revoke these powers for other agents" },
];

export interface CreateWorldAgent {
  name: string;
  persona: string;
  cli: string;
  model: string;
  role: "worker";
  look: Record<string, unknown>;
  gear: Record<string, unknown>;
  visibility: "public" | "private";
  tags?: string[];
  expertise?: string;
}

export interface AgentMessage {
  id: string;
  agent_id: string;
  session_id: string | null;
  author_kind: "user" | "agent" | "system";
  author_user_id: string | null;
  author_agent_id: string | null;
  body: string;
  created_at: string;
  edited_at?: string | null;
  session_role: string | null;
  meta: MessageMeta | null;
  reply_to?: string | null;
  reply_count?: number;
  last_reply_at?: string | null;
  reply_authors?: ReplyAuthor[] | null;
  seen_at?: string | null;
}

export interface ReplyAuthor {
  kind: "user" | "agent" | "system";
  user_id: string | null;
  agent_id: string | null;
  name: string | null;
}

export interface Attachment {
  id: string;
  mime: string;
  name: string;
  url: string;
}

export interface TurnOptions {
  model?: string;
  effort?: string;
  fast?: boolean;
}

export interface Draft {
  to?: string | null;
  subject?: string | null;
  body: string;
  pr?: string | null;
  path?: string | null;
  line?: number | null;
  start_line?: number | null;
  side?: "LEFT" | "RIGHT" | null;
  diff?: string | null;
}

export function draftLabel(d: Draft, i: number): string {
  if (d.path) return `Draft ${i + 1} (review comment on ${d.path}${d.line ? `:${d.line}` : ""}${d.pr ? ` in ${d.pr}` : ""})`;
  return `Draft ${i + 1}${d.to ? ` to ${d.to}` : ""}`;
}

export function draftDecision(drafts: Draft[], decisions: { approved: boolean; body: string }[]): string {
  const lines = drafts.map((d, i) => {
    const label = draftLabel(d, i);
    const pick = decisions[i];
    if (!pick?.approved) return `- ${label}: skip it, do not send.`;
    if (pick.body.trim() === d.body.trim()) return `- ${label}: approved, send it as written.`;
    return `- ${label}: approved with my edits, send exactly this:\n\n${pick.body.trim()}\n`;
  });
  return `My decisions on your drafts:\n${lines.join("\n")}`;
}

export interface MessageMeta {
  kind?: "ask" | "quest" | "voice" | "shell" | "reset" | "equip" | "ui";
  ui?: unknown;
  keys?: string[];
  voice_request?: boolean;
  spoken?: boolean;
  choices?: string[];
  drafts?: Draft[];
  attachments?: Attachment[];
  media?: { collapsed?: boolean };
  options?: TurnOptions | null;
  title?: string;
  status?: string;
  pr_url?: string | null;
  verdict?: string | null;
  error?: string | null;
  cause?: string | null;
  reason?: string | null;
  synthesized?: boolean;
  task_id?: string;
  session_id?: string;
  watching?: boolean;
  merged?: boolean;
  answers?: string;
  shell_run_id?: string;
  command?: string;
  device?: string;
  exit_code?: number | null;
  duration_ms?: number | null;
  truncated?: boolean;
  shell_share?: boolean;
  hidden?: boolean;
  auto_share?: boolean;
  shared?: boolean;
  next?: string[];
  onboarding?: string;
  quick?: string;
  memory?: { line: string };
  memory_answered?: "kept" | "skipped";
  failed?: boolean;
}

export interface QuickBrief {
  id: string;
  agent_id: string;
  title: string | null;
  backend: string;
  model: string;
  status: string;
  thread_root: string | null;
  busy: boolean;
  created_at: string;
  updated_at: string;
}

export interface QuickChat {
  id: string;
  agent_id: string;
  title: string | null;
  backend: string;
  model: string;
  status: string;
  thread_root: string | null;
  busy: boolean;
  messages: AgentMessage[];
}

export const QUICK_KEEP = "Remember";
export const QUICK_SKIP = "Not now";

export interface ShellRun {
  id: string;
  agent_id: string;
  owner_user_id: string;
  runner_id: string | null;
  message_id: string | null;
  command: string;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  output: string;
  truncated: boolean;
  total_bytes: number;
  exit_code: number | null;
  duration_ms: number | null;
  error: string | null;
  cancel_requested: boolean;
  started_at: string | null;
  finished_at: string | null;
  created_at: string;
}

export function answeredAsks(
  messages: { id: string; author_kind: string; author_user_id?: string | null; body: string; meta?: MessageMeta | null }[],
  owner?: string | null,
): Set<string> {
  const answered = new Set<string>();
  const open: { id: string; choices: string[] }[] = [];
  const cards = new Set<string>();
  for (const m of messages) {
    if (m.author_kind === "user" && owner !== undefined && m.author_user_id !== owner) continue;
    if (m.meta?.kind === "ui" && m.author_kind !== "user") {
      cards.add(m.id);
      continue;
    }
    if (m.author_kind === "user" && m.meta?.answers && cards.has(m.meta.answers)) {
      answered.add(m.meta.answers);
      continue;
    }
    if (m.meta?.kind === "ask" && m.author_kind !== "user") {
      open.push({ id: m.id, choices: m.meta.choices ?? [] });
      continue;
    }
    if (m.author_kind !== "user" || !open.length) continue;
    const linked = m.meta?.answers;
    const text = m.body.trim();
    const at = linked
      ? open.findIndex((a) => a.id === linked)
      : (() => {
          for (let i = open.length - 1; i >= 0; i--) if (open[i].choices.some((c) => c.trim() === text)) return i;
          return open.length - 1;
        })();
    if (at < 0) continue;
    answered.add(open[at].id);
    open.splice(at, 1);
  }
  return answered;
}

export interface WorldEvent {
  seq: number;
  type: string;
  agent_id: string | null;
  task_id: string | null;
  run_id?: string | null;
  session_id?: string | null;
  payload: Record<string, unknown>;
  created_at: string;
}

export interface SessionBrief {
  id: string;
  agent_id: string;
  role: "main" | "task" | "aside" | "review" | "designer" | "direct";
  backend: string;
  model: string;
  status: string;
  task_id: string | null;
  task_title: string | null;
  title: string | null;
  summary: string | null;
  cost_usd: number;
  tokens: number;
  runs: number;
  running: boolean;
  created_at: string;
  updated_at: string;
  working_dir?: string | null;
  base_branch?: string | null;
  plan_state?: "planning" | "awaiting" | "approved" | null;
  attempt?: number | null;
  attempt_state?: "running" | "finished" | "failed" | "won" | "lost" | null;
}

export interface AgentUsageRow {
  bucket: string;
  runs: number;
  cost_usd: number;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
}

export interface AgentUsage {
  days: number;
  total: AgentUsageRow;
  per_day: AgentUsageRow[];
  per_kind: AgentUsageRow[];
  per_backend: AgentUsageRow[];
}

export interface SessionRun {
  id: string;
  kind: string;
  status: string;
  prompt: string;
  output: string | null;
  error: string | null;
  cost_usd: number;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  created_at: string;
  finished_at: string | null;
  retryable: boolean;
  retry_mode: "resume" | "restart" | null;
  steered_into?: string | null;
  interrupt?: boolean;
}

export interface RetryRunResult {
  run: SessionRun | null;
  retry_mode: "resume" | "restart";
}

export interface RunEvent {
  id: number;
  kind: "tool" | "result" | "text" | "error";
  tool: string | null;
  body: string;
  created_at: string;
}

export interface ChannelSummary {
  id: string;
  name: string;
  topic: string | null;
  kind: "channel" | "dm" | "team";
  member: boolean;
  unread: number;
  last_message_at: string | null;
  peers?: string | null;
}

export interface ChannelMember {
  member_kind: "user" | "agent";
  member_id: string;
  name: string;
  emoji: string | null;
}

export interface ChannelDetail {
  id: string;
  name: string;
  topic: string | null;
  kind: "channel" | "dm" | "team";
  members: ChannelMember[];
}

export interface ChannelMessage {
  id: number;
  channel_id: string;
  author_kind: "user" | "agent" | "system";
  author_user_id: string | null;
  author_agent_id: string | null;
  author_name: string | null;
  author_emoji: string | null;
  body: string;
  created_at: string;
  edited_at?: string | null;
  meta: MessageMeta | null;
  reply_to?: number | null;
  reply_count?: number;
  last_reply_at?: string | null;
  reply_authors?: ReplyAuthor[] | null;
  seen_by?: string[];
}

export interface Person {
  id: string;
  name: string | null;
  email: string | null;
}

export interface Todo {
  id: string;
  owner_user_id: string;
  text: string;
  priority: number;
  position: number | null;
  added_by_agent: string | null;
  added_by_user: string | null;
  source: { url?: string; label?: string | null; suggested?: boolean } | null;
  due_at: string | null;
  nudge_at: string | null;
  nudge_count: number;
  repeat_nudge: boolean;
  last_nudged_at: string | null;
  status: "open" | "done" | "dismissed";
  done_at: string | null;
  started_at: string | null;
  added_at: string;
  updated_at: string;
  proposals?: TodoProposal[];
  decision?: { index: number; kind: string; label: string; edited: boolean; agent_id: string; at: string; picked?: { index: number; kind: string; label: string; edited: boolean }[] } | null;
}

export interface TodoProposal {
  kind: "reply" | "message" | "task" | "other";
  label: string;
  to?: string | null;
  via?: string | null;
  title?: string | null;
  agent?: string | null;
  repo?: string | null;
  body: string;
}

export type TodoPatch = { status?: Todo["status"]; text?: string; priority?: number; due_at?: string | null; nudge_at?: string | null; repeat_nudge?: boolean; started?: boolean };

export interface WorldItem {
  id: string;
  kind: "building" | "decoration" | "critter" | "timer" | "list" | "portal" | "npc";
  name: string;
  owner_user_id: string | null;
  created_by_agent: string | null;
  data: Record<string, unknown>;
  status: "active" | "done" | "archived";
  fires_at: string | null;
  created_at: string;
  updated_at: string;
  done_count?: number;
}

export interface WorldRule {
  id: string;
  user_id: string | null;
  scope: "all" | "talk" | "work";
  body: string;
  enabled: boolean;
  position: number;
}

export interface EnvironmentEntry {
  name: string;
  updated_at: string;
  agent_ids: string[];
}

export interface WantedKeys {
  agent_id: string;
  agent_name: string;
  keys: string[];
}

export type ProviderKeyStatus = "own" | "shared" | "missing";

export interface ProviderKey {
  id: string;
  label: string;
  env: string;
  unlocks: string;
  docs: string;
  hint: string;
  testable: boolean;
  status: ProviderKeyStatus;
}

export interface ProviderCheck {
  problem: string | null;
  live: "ok" | "failed" | "skipped";
  detail: string | null;
}

export interface EnvironmentVariant {
  name: string;
  environment: string;
  updated_at: string;
}

export interface EnvironmentChoice {
  agent_id: string;
  name: string;
  environment: string;
  expires_at: string | null;
  chosen_by: string;
  chosen_at: string;
}

export interface EnvironmentResponse {
  configured: boolean;
  entries: EnvironmentEntry[];
  wanted: WantedKeys[];
  providers?: ProviderKey[];
  variants?: EnvironmentVariant[];
  choices?: EnvironmentChoice[];
}

export interface CommandRule {
  id: string;
  agent_id: string | null;
  kind: "allow" | "deny";
  rule: string;
  created_by: string;
  created_at: string;
}

export interface SharedKey {
  name: string;
  note: string;
  updated_at: string;
  agent_ids: string[];
}

export interface SharedKeysResponse {
  configured: boolean;
  keys: SharedKey[];
}

export interface AgentFeed {
  id: string;
  agent_id: string;
  kind: "rss" | "webhook";
  name: string;
  url: string | null;
  webhook_url?: string;
  format: string;
  filter: string | null;
  poll_secs: number;
  next_poll_at: string | null;
  last_item_at: string | null;
  last_error: string | null;
  item_count: number;
  paused: boolean;
  created_at: string;
  allowed_ips?: string[];
  auth?: "verified" | "waiting" | "grace" | "refusing";
  auth_grace_until?: string | null;
  has_secret?: boolean;
  secret_unseen?: boolean;
  auth_mode?: "secret" | "address";
  intake?: FeedIntake;
  checks?: string;
  last_fields?: string[];
  turned_away?: number;
  turned_away_why?: string | null;
}

export interface FeedIntake {
  source?: string;
  require?: string[];
  skip?: string[];
  keep?: string[];
  title?: string;
  link?: string[];
  json_only?: boolean;
}

export interface FeedSecret {
  feed: AgentFeed;
  secret: string | null;
  setup?: string;
  examples: { curl: string; alertmanager?: string; github?: string };
}

export interface NewWebhookFeed {
  name?: string;
  source: string;
  auth?: "secret" | "address";
  allowed_ips?: string[];
  require?: string[];
  keep?: string[];
}

async function feedJson<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error ?? `HTTP ${res.status}`);
  }
  return (await res.json()) as T;
}

export interface AgentSchedule {
  id: string;
  agent_id: string;
  name: string;
  prompt: string;
  cron_expression: string | null;
  run_at: string | null;
  next_run_at: string | null;
  enabled: boolean;
  kind?: "routine" | "heartbeat";
}

export interface McpServer {
  id: string;
  name: string;
  definition: Record<string, unknown>;
  scope: "global" | "agent";
  enabled: boolean;
  duplicate_of?: string;
}

export interface Skill {
  id: string;
  name: string;
  description: string;
  body?: string;
  source_url: string | null;
  scope: "global" | "agent";
  enabled: boolean;
}

export interface TaskBrief {
  id: string;
  title: string;
  status: string;
  repo: string | null;
  pr_url: string | null;
  assigned_agent_id: string | null;
  assignee_user_id: string | null;
  result_content: string | null;
  user_id?: string | null;
  milestones?: Milestone[];
  plan_first?: boolean;
  attempts?: number | null;
  plan_waiting?: boolean;
  pace?: QuestPace | null;
}

export type QuestPace = "normal" | "quick" | "urgent";

export interface QuestTiming {
  queued_secs?: number;
  work_secs?: number;
  review_secs?: number;
  publish_secs?: number;
  total_secs?: number;
}

export interface QuestOptions {
  plan_first?: boolean;
  attempts?: number;
  ci_feedback?: boolean;
  pace?: QuestPace;
}

export interface QuestPart {
  id: string;
  title: string;
  status: string;
  assigned_agent_id: string | null;
  pr_url: string | null;
}

export interface QuestRoomTask {
  parts?: QuestPart[];
  parent?: QuestPart | null;
  id: string;
  title: string;
  description: string | null;
  status: string;
  repo: string | null;
  branch: string | null;
  pr_url: string | null;
  assigned_agent_id: string | null;
  user_id: string | null;
  result_content: string | null;
  error_message: string | null;
  created_at: string | null;
  updated_at: string | null;
  milestones: Milestone[];
  plan_first?: boolean;
  attempts?: number;
  ci_feedback?: boolean;
  pace?: QuestPace;
}

export interface QuestRoomPage {
  task: QuestRoomTask;
  room: {
    sessions: SessionBrief[];
    runs: (SessionRun & { session_id: string; agent_id: string })[];
    events: WorldEvent[];
    messages: AgentMessage[];
    cursor: number;
  };
}

export interface QuestPatch extends QuestOptions {
  status?: string;
  time_limit_minutes?: number | null;
  assigned_agent_id?: string | null;
  assignee_user_id?: string | null;
}

export interface Milestone {
  id: string;
  task_id: string;
  position: number;
  title: string;
  kind: "work" | "merge";
  status: "pending" | "active" | "done" | "failed" | "skipped";
  detail: string | null;
  next_check_at: string | null;
  done_at: string | null;
}

export interface MilestoneDraft {
  title: string;
  kind: "work" | "merge";
}

export interface RunnerView {
  id: string;
  name: string;
  platform: string;
  status: "online" | "offline" | "paused";
  last_seen_at: string | null;
  owner_user_id: string;
  capabilities: Record<string, unknown>;
  created_at: string;
  shared?: boolean;
  screen_access?: boolean;
  app_control?: boolean;
}

export type ReadinessState = "ready" | "needs_setup" | "unavailable";
export type SetupAction = "devices" | "keys" | "github";
export type FeatureKey = "devices" | "clis" | "terminal" | "workshop" | "browser" | "screen" | "apps" | "slack" | "github_feed" | "live_voice" | "keys" | "opencode";

export interface FeatureReadiness {
  key: FeatureKey;
  label: string;
  state: ReadinessState;
  reason: string;
  step: string | null;
  action: SetupAction | null;
  fix?: SetupFix | null;
}

export type SetupFix =
  | { kind: "switch"; setting: "app_control" | "screen_access" | "github_feed" | "slack_capture"; runner_id: string | null; label: string }
  | { kind: "command"; command: string; runner_id: string; runner: string; interactive: boolean };

export interface Readiness {
  features: FeatureReadiness[];
  clis: string[];
}

export class WorldApiError extends Error {
  constructor(
    public status: number,
    message: string,
    public code?: string,
  ) {
    super(message);
  }
}

export function messageTime(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

export function sameDay(a: string, b: string): boolean {
  return new Date(a).toDateString() === new Date(b).toDateString();
}

export function dayLabel(iso: string, now = new Date()): string {
  const d = new Date(iso);
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const day = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const daysAgo = Math.round((today.getTime() - day.getTime()) / 86_400_000);
  if (daysAgo === 0) return "Today";
  if (daysAgo === 1) return "Yesterday";
  if (daysAgo > 1 && daysAgo < 7) return d.toLocaleDateString(undefined, { weekday: "long" });
  return d.toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    ...(d.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

export function compactNumber(n: number): string {
  const units = ["", "K", "M", "B"];
  let value = n;
  let unit = 0;
  const rounded = (v: number) => Number(v.toFixed(Math.abs(v) >= 100 || unit === 0 ? 0 : 1));
  while (unit < units.length - 1 && Math.abs(rounded(value)) >= 1000) {
    value /= 1000;
    unit += 1;
  }
  return `${rounded(value)}${units[unit]}`;
}

export function formatUsage(cost: number, tokens: number, partialCost = false): string {
  const parts: string[] = [];
  if (tokens > 0) parts.push(`${compactNumber(tokens)} tokens`);
  if (cost > 0) {
    const amount = cost < 0.001 ? "<$0.001" : cost >= 1 ? `$${cost.toFixed(2)}` : `$${cost.toFixed(3)}`;
    parts.push(partialCost ? (cost < 0.001 ? `Recorded cost ${amount}` : `Cost at least ${amount}`) : `Cost ${amount}`);
  }
  return parts.join(" · ");
}

export interface OmpSecret {
  name: string;
  in_environment: boolean;
  granted: boolean;
}

export interface OmpConfig {
  config: string | null;
  masked: boolean;
  secrets: OmpSecret[];
  models: { id: string; name: string | null }[];
  private: boolean;
  can_equip: boolean;
  environment_configured: boolean;
}

async function ompJson(res: Response): Promise<OmpConfig> {
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error ?? `HTTP ${res.status}`);
  }
  return (await res.json()) as OmpConfig;
}

export type ApiToken = {
  id: string;
  name: string;
  prefix: string;
  scope: "read" | "write";
  agent_ids: string[] | null;
  created_at: string;
  expires_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
};

export type CreatedApiToken = ApiToken & { secret: string };

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

async function quickJson<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new WorldApiError(res.status, body?.error ?? "Something went wrong. Try again.");
  }
  return res.json() as Promise<T>;
}

async function retryRun(id: string): Promise<RetryRunResult> {
  const res = await fetch(`/api/world/runs/${id}/retry`, { method: "POST" });
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(`${res.status} ${body?.error ?? res.statusText}`);
  }
  return res.json() as Promise<RetryRunResult>;
}

export interface BellItem {
  id: number;
  seq: number;
  kind: "ask" | "message" | "mention" | "reminder" | "quest" | "alert";
  agent_id: string | null;
  created_at: string;
  cleared_at: string | null;
}

export interface Notifications {
  items?: BellItem[];
  events: WorldEvent[];
  unread: number;
  unread_seqs?: number[];
  by_agent?: Record<string, number>;
}

export interface SearchMessageHit {
  ref: string;
  kind: "agent" | "channel";
  agent_id: string | null;
  agent_name: string | null;
  agent_emoji: string | null;
  channel_id: string | null;
  channel_name: string | null;
  channel_kind: string | null;
  author: string;
  author_kind: string;
  author_emoji: string | null;
  excerpt: string;
  created_at: string;
}

export interface SearchResults {
  agents: { id: string; name: string; slug: string; avatar_emoji: string; expertise: string | null; tags: string[] }[];
  messages: SearchMessageHit[];
  reports?: { id: string; title: string; folder: string; visibility: string; author_agent_id: string | null; excerpt: string; updated_at: string }[];
  quests: { id: string; title: string; status: string; assigned_agent_id: string | null; updated_at: string | null }[];
  todos: { id: string; text: string; status: string; priority: number; added_at: string }[];
  lists: { id: string; kind?: "list" | "timer"; name: string; status: string; updated_at: string }[];
  channels: { id: string; name: string; topic: string | null }[];
  schedules?: { id: string; agent_id: string; name: string; kind: string; cron_expression: string | null; enabled: boolean; next_run_at: string | null }[];
  feeds?: { id: string; agent_id: string; name: string; kind: string; paused: boolean }[];
  teams?: { id: string; name: string; purpose: string; lead_agent_id: string | null }[];
  regions?: { id: string; name: string; theme: string; visibility: string; mine: boolean }[];
  places?: { id: string; kind: "building" | "npc"; name: string; region_id: string | null; region_name: string | null; data: Record<string, unknown> }[];
  rules?: { id: string; scope: string; excerpt: string; enabled: boolean; global: boolean }[];
  kit?: { id: string; kind: "skill" | "mcp"; name: string; description: string; agent_id: string }[];
  devices?: { id: string; name: string; platform: string; status: string }[];
  people?: { id: string; name: string | null; email: string; role: string }[];
}

export function matchesQuery(query: string, ...fields: (string | null | undefined)[]): boolean {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!terms.length) return true;
  const hay = fields.filter(Boolean).join(" ").toLowerCase();
  return terms.every((term) => hay.includes(term));
}

export interface TerminalOpened {
  session_id: string;
  device: string;
  socket: string;
  expires_at: number;
}

export const worldApi = {
  search: (q: string, signal?: AbortSignal) =>
    fetch(`/api/world/search?q=${encodeURIComponent(q)}&limit=8`, { cache: "no-store", signal }).then((r) => json<SearchResults>(r)),
  agents: () => fetch("/api/world/agents", { cache: "no-store" }).then((r) => json<WorldAgent[]>(r)),
  readiness: () => fetch("/api/world/readiness", { cache: "no-store" }).then((r) => json<Readiness>(r)),
  runners: (signal?: AbortSignal) => fetch("/api/world/runners", { cache: "no-store", signal }).then((r) => json<RunnerView[]>(r)),
  setDeviceAccess: async (id: string, change: { screen_access?: boolean; app_control?: boolean }) => {
    const res = await fetch(`/api/world/runners/${id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(change),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string; message?: string } | null;
      throw new WorldApiError(res.status, body?.message ?? res.statusText, body?.error);
    }
    return res.json() as Promise<RunnerView>;
  },
  openTerminal: async (runnerId: string, cols: number, rows: number) => {
    const res = await fetch("/api/world/terminal/sessions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ runner_id: runnerId, cols, rows }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string; message?: string } | null;
      throw new WorldApiError(res.status, body?.message ?? res.statusText, body?.error);
    }
    return res.json() as Promise<TerminalOpened>;
  },
  agentRunner: (id: string) =>
    fetch(`/api/world/agents/${id}/runner`, { cache: "no-store" }).then((r) => json<{ runner: RunnerView | null }>(r)),
  setAgentRunner: async (id: string, runnerId: string | null) => {
    const res = await fetch(`/api/world/agents/${id}/runner`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ runner_id: runnerId }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string; message?: string } | null;
      throw new WorldApiError(res.status, body?.message ?? res.statusText, body?.error);
    }
    return res.json() as Promise<{ runner: RunnerView | null }>;
  },
  handoffAgent: async (id: string, runnerId: string, cli: string, model: string, summary: string) => {
    const res = await fetch(`/api/world/agents/${id}/handoff`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ runner_id: runnerId, cli, model, summary }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string; message?: string } | null;
      throw new WorldApiError(res.status, body?.message ?? res.statusText, body?.error);
    }
    return res.json() as Promise<{ runner: RunnerView | null }>;
  },
  createAgent: (agent: CreateWorldAgent) =>
    fetch("/api/world/agents", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(agent),
    }).then((r) => json<WorldAgent>(r)),
  runCommand: (id: string, command: string, runOn?: string) =>
    fetch(`/api/world/agents/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body: `!${command}`, share_output: true, run_on: runOn }),
    }).then((r) => json<AgentMessage>(r)),
  messages: (id: string) =>
    fetch(`/api/world/agents/${id}/messages?limit=200`, { cache: "no-store" }).then((r) => json<AgentMessage[]>(r)),
  messagesBefore: (id: string, before: string) =>
    fetch(`/api/world/agents/${id}/messages?limit=200&before=${encodeURIComponent(before)}`, { cache: "no-store" }).then((r) => json<AgentMessage[]>(r)),
  send: (id: string, body: string, attachments: string[] = [], options?: TurnOptions, answers?: string, spoken?: boolean, taskId?: string, replyTo?: string) =>
    fetch(`/api/world/agents/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body, attachments, options, answers, spoken, task_id: taskId, reply_to: replyTo }),
    }).then((r) => json<AgentMessage>(r)),
  quickStart: (agentId: string, body: { text: string; attachments?: string[]; spoken?: boolean; cli?: string; model?: string }) =>
    fetch(`/api/world/agents/${agentId}/quick`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => quickJson<{ session_id: string; run_id: string; message: AgentMessage }>(r)),
  quickList: (opts: { agent?: string; threads?: boolean } = {}) => {
    const q = new URLSearchParams();
    if (opts.agent) q.set("agent", opts.agent);
    if (opts.threads) q.set("threads", "true");
    return fetch(`/api/world/quick?${q.toString()}`, { cache: "no-store" }).then((r) => quickJson<QuickBrief[]>(r));
  },
  quickRead: (id: string) => fetch(`/api/world/quick/${id}`, { cache: "no-store" }).then((r) => quickJson<QuickChat>(r)),
  quickSend: (id: string, body: { text: string; attachments?: string[]; spoken?: boolean; answers?: string }) =>
    fetch(`/api/world/quick/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => quickJson<{ session_id: string; message: AgentMessage }>(r)),
  quickClose: (id: string) =>
    fetch(`/api/world/quick/${id}/close`, { method: "POST" }).then((r) => {
      if (!r.ok) throw new WorldApiError(r.status, "Could not close that quick chat.");
    }),
  messageReplies: (id: string, messageId: string) =>
    fetch(`/api/world/agents/${id}/messages/${messageId}/replies`, { cache: "no-store" }).then((r) => json<AgentMessage[]>(r)),
  questRoom: (taskId: string, after?: number, since?: string) => {
    const q = new URLSearchParams();
    if (after !== undefined) q.set("after", String(after));
    if (since) q.set("since", since);
    const qs = q.toString();
    return fetch(`/api/world/tasks/${taskId}/room${qs ? `?${qs}` : ""}`, { cache: "no-store" }).then((r) => json<QuestRoomPage>(r));
  },
  editMessage: (id: string, messageId: string, body: string) =>
    fetch(`/api/world/agents/${id}/messages/${messageId}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body }),
    }).then((r) => json<AgentMessage>(r)),
  rewind: async (id: string, messageId: string, resend?: { body: string; attachments: string[]; options?: TurnOptions }) => {
    const res = await fetch(`/api/world/agents/${id}/messages/${messageId}/rewind`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ resend: resend ?? null }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string } | null;
      throw new Error(body?.error ?? "Could not rewind. Try again.");
    }
    return res.json() as Promise<{ deleted: string[]; message: AgentMessage | null }>;
  },
  upload: (file: Blob, name: string) =>
    fetch(`/api/world/uploads?name=${encodeURIComponent(name)}`, {
      method: "POST",
      headers: { "content-type": file.type },
      body: file,
    }).then((r) => json<Attachment>(r)),
  shellRun: (id: string, signal?: AbortSignal) =>
    fetch(`/api/world/shell/${id}`, { cache: "no-store", signal }).then((r) => json<ShellRun>(r)),
  cancelShell: (id: string) =>
    fetch(`/api/world/shell/${id}/cancel`, { method: "POST" }).then((r) => json<ShellRun>(r)),
  shareShell: async (id: string) => {
    const res = await fetch(`/api/world/shell/${id}/share`, { method: "POST" });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string; message?: string } | null;
      throw new WorldApiError(res.status, body?.message ?? res.statusText, body?.error);
    }
    return res.json() as Promise<{ message_id: string }>;
  },
  aside: (id: string, question: string, choice?: { cli?: string; model?: string }, spoken?: boolean) =>
    fetch(`/api/world/agents/${id}/asides`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ question, ...choice, spoken }),
    }).then((r) => json<{ session_id: string }>(r)),
  startDirect: async (id: string, body: { folder_id: string; folder_name?: string; text?: string; cli?: string; model?: string }) => {
    const res = await fetch(`/api/world/agents/${id}/direct`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const problem = (await res.json().catch(() => null)) as { error?: string } | null;
      throw new WorldApiError(res.status, problem?.error ?? "Could not start the session.");
    }
    return res.json() as Promise<{ session_id: string; run_id: string | null; runner: string }>;
  },
  startOnboarding: () =>
    fetch("/api/world/onboarding", { method: "POST" }).then((r) => json<{ pending: boolean; agent_id: string | null }>(r)),
  ensureAssistant: () =>
    fetch("/api/world/assistant", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" }).then((r) =>
      json<WorldAgent>(r),
    ),
  update: (id: string, patch: Partial<Pick<WorldAgent, "name" | "persona" | "cli" | "model" | "avatar_emoji" | "look" | "gear" | "tags" | "expertise" | "revoked_powers" | "visibility" | "approval_mode" | "reviewer" | "daily_budget_usd" | "daily_token_budget">> & { handle?: string; paused?: boolean; archived?: boolean; delete_after_days?: number | null }) =>
    fetch(`/api/world/agents/${id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    }).then(async (r) => {
      if (!r.ok) {
        const body = (await r.json().catch(() => null)) as { error?: string; message?: string } | null;
        throw new WorldApiError(r.status, body?.message ?? "", body?.error);
      }
      return r.json() as Promise<WorldAgent>;
    }),
  remove: (id: string) =>
    fetch(`/api/world/agents/${id}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  usage: (id: string, days?: number) =>
    fetch(`/api/world/agents/${id}/usage${days ? `?days=${days}` : ""}`, { cache: "no-store" }).then((r) => json<AgentUsage>(r)),
  subscriptions: (id: string) => fetch(`/api/world/agents/${id}/subscriptions`, { cache: "no-store" }).then((r) => json<string[]>(r)),
  watches: (id: string) => fetch(`/api/world/agents/${id}/watches`, { cache: "no-store" }).then((r) => json<AgentWatch[]>(r)),
  removeWatch: async (id: string, watch: string) => {
    const res = await fetch(`/api/world/agents/${id}/watches/${watch}`, { method: "DELETE" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
  },
  setSubscriptions: (id: string, eventTypes: string[]) =>
    fetch(`/api/world/agents/${id}/subscriptions`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ event_types: eventTypes }),
    }).then((r) => json<string[]>(r)),
  schedules: (id: string) => fetch(`/api/world/agents/${id}/schedules`, { cache: "no-store" }).then((r) => json<AgentSchedule[]>(r)),
  addSchedule: (id: string, input: { name: string; prompt: string; cron_expression?: string; run_at?: string; kind?: "routine" | "heartbeat" }) =>
    fetch(`/api/world/agents/${id}/schedules`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(input),
    }).then((r) => json<AgentSchedule>(r)),
  toggleSchedule: (id: string, scheduleId: string, enabled: boolean) =>
    fetch(`/api/world/agents/${id}/schedules/${scheduleId}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ enabled }),
    }),
  updateSchedule: async (id: string, scheduleId: string, input: { prompt?: string; cron_expression?: string; enabled?: boolean }) => {
    const r = await fetch(`/api/world/agents/${id}/schedules/${scheduleId}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(input),
    });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
  },
  feeds: (id: string) => fetch(`/api/world/agents/${id}/feeds`, { cache: "no-store" }).then((r) => json<AgentFeed[]>(r)),
  pauseFeed: (id: string, feedId: string, paused: boolean) =>
    fetch(`/api/world/agents/${id}/feeds/${feedId}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ paused }),
    }).then((r) => json<AgentFeed>(r)),
  createWebhookFeed: (id: string, body: NewWebhookFeed) =>
    fetch(`/api/world/agents/${id}/feeds`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => feedJson<FeedSecret>(r)),
  revealFeedSecret: (id: string, feedId: string) =>
    fetch(`/api/world/agents/${id}/feeds/${feedId}/secret/reveal`, { method: "POST" }).then((r) => feedJson<FeedSecret>(r)),
  rotateFeedSecret: (id: string, feedId: string) =>
    fetch(`/api/world/agents/${id}/feeds/${feedId}/secret`, { method: "POST" }).then((r) => feedJson<FeedSecret>(r)),
  updateFeedChecks: (id: string, feedId: string, body: { auth?: "secret" | "address"; require?: string[]; keep?: string[] }) =>
    fetch(`/api/world/agents/${id}/feeds/${feedId}`, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => feedJson<AgentFeed>(r)),
  setFeedRanges: (id: string, feedId: string, allowed_ips: string[]) =>
    fetch(`/api/world/agents/${id}/feeds/${feedId}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ allowed_ips }),
    }).then((r) => feedJson<AgentFeed>(r)),
  removeFeed: (id: string, feedId: string) =>
    fetch(`/api/world/agents/${id}/feeds/${feedId}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  deleteSchedule: (id: string, scheduleId: string) => fetch(`/api/world/agents/${id}/schedules/${scheduleId}`, { method: "DELETE" }),
  kit: (id: string) => fetch(`/api/world/agents/${id}/kit`, { cache: "no-store" }).then((r) => json<{ mcp_servers: McpServer[]; skills: Skill[] }>(r)),
  ompConfig: (id: string) => fetch(`/api/world/agents/${id}/omp-config`, { cache: "no-store" }).then(ompJson),
  saveOmpConfig: (id: string, config: string) =>
    fetch(`/api/world/agents/${id}/omp-config`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ config }),
    }).then(ompJson),
  deleteOmpConfig: (id: string) =>
    fetch(`/api/world/agents/${id}/omp-config`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  equipOmpSecrets: (id: string, names: string[]) =>
    fetch(`/api/world/agents/${id}/omp-config/equip`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ names }),
    }).then(ompJson),
  mcpLibrary: () => fetch("/api/world/mcp-servers", { cache: "no-store" }).then((r) => json<McpServer[]>(r)),
  skillLibrary: () => fetch("/api/world/skills", { cache: "no-store" }).then((r) => json<Skill[]>(r)),
  attach: (id: string, kind: "mcp-servers" | "skills", itemId: string, on: boolean) =>
    fetch(`/api/world/agents/${id}/${kind}/${itemId}`, { method: on ? "POST" : "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  installMcp: (agentId: string, name: string, definition: Record<string, unknown>) =>
    fetch("/api/world/mcp-servers", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name, definition, agent_id: agentId }),
    }).then((r) => json<McpServer>(r)),
  installSkill: (agentId: string, source: { source_url?: string; body?: string }) =>
    fetch("/api/world/skills", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ ...source, agent_id: agentId }),
    }).then((r) => json<Skill>(r)),
  backends: () =>
    fetch("/api/config", { cache: "no-store" })
      .then((r) => json<{ backends?: Record<string, BackendModels> }>(r))
      .then((c) => c.backends ?? {}),
  sessions: (agentId?: string) =>
    fetch(`/api/world/sessions${agentId ? `?agent_id=${agentId}` : ""}`, { cache: "no-store" }).then((r) => json<SessionBrief[]>(r)),
  sessionRuns: (id: string, before?: string, light = false) =>
    fetch(`/api/world/sessions/${id}/runs?limit=${RUN_PAGE}${before ? `&before=${encodeURIComponent(before)}` : ""}${light ? "&light=true" : ""}`, { cache: "no-store" }).then((r) => json<SessionRun[]>(r)),
  runText: (id: string) => fetch(`/api/world/runs/${id}/text`, { cache: "no-store" }).then((r) => json<{ prompt: string; output: string | null }>(r)),
  tidyHistory: (id: string) => fetch(`/api/world/agents/${id}/tidy`, { method: "POST" }).then((r) => json<{ removed: number }>(r)),
  resetContext: (id: string) => fetch(`/api/world/agents/${id}/reset-context`, { method: "POST" }).then((r) => json<{ queued: boolean }>(r)),
  runEvents: (id: string, after?: number) =>
    fetch(`/api/world/runs/${id}/events${after !== undefined ? `?after=${after}` : ""}`, { cache: "no-store" }).then((r) => json<RunEvent[]>(r)),
  retryRun,
  stopRun: async (id: string, mode: "step" | "run") => {
    const res = await fetch(`/api/world/runs/${id}/stop`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ mode }),
    });
    if (!res.ok) throw new Error(res.status === 409 ? "That run already finished." : "Could not stop it. Try again.");
  },
  followUp: (id: string, text: string, interrupt = false) =>
    fetch(`/api/world/sessions/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ text, interrupt }),
    }).then((r) => json<SessionRun>(r)),
  review: (id: string, reviewer: { agentId?: string | null; cli?: string | null; model?: string | null }, focus: string | null) =>
    fetch(`/api/world/sessions/${id}/review`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ reviewer_agent_id: reviewer.agentId ?? null, reviewer_cli: reviewer.cli ?? null, reviewer_model: reviewer.model ?? null, focus }),
    }).then((r) => json<SessionRun>(r)),
  notifications: (cleared = false) =>
    fetch(`/api/world/notifications${cleared ? "?cleared=true" : ""}`, { cache: "no-store" }).then((r) => json<Notifications>(r)),
  clearNotes: (body: { ids?: number[]; all?: boolean }) =>
    fetch("/api/world/notifications/clear", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => {
      if (!r.ok) throw new Error(`clear failed: ${r.status}`);
    }),
  seenAgent: (agentId: string) =>
    fetch("/api/world/notifications/seen", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agent_id: agentId }),
    }),
  channels: () => fetch("/api/world/channels", { cache: "no-store" }).then((r) => json<ChannelSummary[]>(r)),
  channel: (id: string) => fetch(`/api/world/channels/${id}`, { cache: "no-store" }).then((r) => json<ChannelDetail>(r)),
  createChannel: (name: string, topic: string) =>
    fetch("/api/world/channels", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name, topic }),
    }).then((r) => json<ChannelDetail>(r)),
  openDm: (userId: string) =>
    fetch("/api/world/dms", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ user_id: userId }),
    }).then((r) => json<ChannelDetail>(r)),
  channelMessages: (id: string, after?: number) =>
    fetch(`/api/world/channels/${id}/messages${after ? `?after=${after}` : ""}`, { cache: "no-store" }).then((r) =>
      json<ChannelMessage[]>(r),
    ),
  channelMessagesBefore: (id: string, before: number) =>
    fetch(`/api/world/channels/${id}/messages?before=${before}`, { cache: "no-store" }).then((r) => json<ChannelMessage[]>(r)),
  postChannel: (id: string, body: string, attachments: string[] = [], replyTo?: number, spoken?: boolean) =>
    fetch(`/api/world/channels/${id}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body, attachments, reply_to: replyTo, spoken }),
    }).then((r) => json<ChannelMessage>(r)),
  channelReplies: (id: string, messageId: number) =>
    fetch(`/api/world/channels/${id}/messages/${messageId}/replies`, { cache: "no-store" }).then((r) => json<ChannelMessage[]>(r)),
  editChannelMessage: (id: string, messageId: number, body: string) =>
    fetch(`/api/world/channels/${id}/messages/${messageId}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body }),
    }).then((r) => json<ChannelMessage>(r)),
  deleteChannelMessage: async (id: string, messageId: number) => {
    const res = await fetch(`/api/world/channels/${id}/messages/${messageId}`, { method: "DELETE" });
    if (!res.ok) throw new Error(String(res.status));
  },
  addMember: (id: string, kind: "user" | "agent", memberId: string) =>
    fetch(`/api/world/channels/${id}/members`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ kind, id: memberId }),
    }),
  markRead: (id: string, lastId: number) =>
    fetch(`/api/world/channels/${id}/read`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ last_id: lastId }),
    }),
  people: () => fetch("/api/users", { cache: "no-store" }).then((r) => json<Person[]>(r)),
  apiTokens: () => fetch("/api/world/tokens", { cache: "no-store" }).then((r) => json<ApiToken[]>(r)),
  createApiToken: async (name: string, scope: "read" | "write", agentIds: string[] | null, expiresInDays: number) => {
    const res = await fetch("/api/world/tokens", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name, scope, agent_ids: agentIds, expires_in_days: expiresInDays }),
    });
    if (!res.ok) {
      const body = (await res.json().catch(() => null)) as { error?: string } | null;
      throw new Error(body?.error ?? `HTTP ${res.status}`);
    }
    return (await res.json()) as CreatedApiToken;
  },
  revokeApiToken: async (id: string) => {
    const res = await fetch(`/api/world/tokens/${id}`, { method: "DELETE" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
  },
  github: () => fetch("/api/world/github", { cache: "no-store" }).then((r) => json<GithubSettings>(r)),
  setGithub: (enabled: boolean) =>
    fetch("/api/world/github", { method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify({ enabled }) }).then((r) => json<GithubSettings>(r)),
  setMentionWakesMain: async (on: boolean) => {
    const res = await fetch("/api/world/me", { method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ mention_wakes_main: on }) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
  },
  rules: () => fetch("/api/world/rules", { cache: "no-store" }).then((r) => json<WorldRule[]>(r)),
  addRule: (body: string, scope: string, global: boolean) =>
    fetch("/api/world/rules", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ body, scope, global }),
    }).then((r) => json<WorldRule>(r)),
  updateRule: (id: string, patch: { body?: string; scope?: string; enabled?: boolean }) =>
    fetch(`/api/world/rules/${id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    }).then((r) => json<WorldRule>(r)),
  deleteRule: (id: string) => fetch(`/api/world/rules/${id}`, { method: "DELETE" }),
  environment: () => fetch("/api/world/environment", { cache: "no-store" }).then((r) => json<EnvironmentResponse>(r)),
  saveEnvironment: (name: string, value: string) =>
    fetch(`/api/world/environment/${encodeURIComponent(name)}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ value }),
    }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  saveEnvironmentVariant: (name: string, environment: string, value: string) =>
    fetch(`/api/world/environment/${encodeURIComponent(name)}/variants/${encodeURIComponent(environment)}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ value }),
    }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  deleteEnvironmentVariant: (name: string, environment: string) =>
    fetch(`/api/world/environment/${encodeURIComponent(name)}/variants/${encodeURIComponent(environment)}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  chooseKeyEnvironment: (body: { agent_id: string; name?: string; environment: string; hours?: number }) =>
    fetch("/api/world/environment/choices", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => json<{ keys: string[] }>(r)),
  commandRules: () => fetch("/api/world/command-rules", { cache: "no-store" }).then((r) => json<{ rules: CommandRule[] }>(r)),
  addCommandRule: async (body: { agent_id?: string | null; kind: "allow" | "deny"; rule: string }) => {
    const r = await fetch("/api/world/command-rules", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!r.ok) {
      const detail = await r.json().catch(() => null);
      throw new Error(detail?.error ?? "Could not add that rule.");
    }
    return (await r.json()) as CommandRule;
  },
  deleteCommandRule: (id: string) =>
    fetch(`/api/world/command-rules/${id}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  checkProviderKey: (id: string, body: { value?: string; live?: boolean }) =>
    fetch(`/api/world/environment/providers/${encodeURIComponent(id)}/check`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => json<ProviderCheck>(r)),
  setEnvironmentAgents: (name: string, agentIds: string[]) =>
    fetch(`/api/world/environment/${encodeURIComponent(name)}/agents`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agent_ids: agentIds }),
    }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  dismissWantedKey: (agentId: string, name: string) =>
    fetch(`/api/world/environment/wanted/${agentId}/${encodeURIComponent(name)}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  fork: (id: string, name?: string) =>
    fetch(`/api/world/agents/${id}/fork`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name }),
    }).then((r) => json<WorldAgent>(r)),
  sharedKeys: () => fetch("/api/world/shared-keys", { cache: "no-store" }).then((r) => json<SharedKeysResponse>(r)),
  saveSharedKey: (name: string, body: { value?: string; note?: string }) =>
    fetch(`/api/world/shared-keys/${encodeURIComponent(name)}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  setSharedKeyAgents: (name: string, agentIds: string[]) =>
    fetch(`/api/world/shared-keys/${encodeURIComponent(name)}/agents`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ agent_ids: agentIds }),
    }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  deleteSharedKey: (name: string) =>
    fetch(`/api/world/shared-keys/${encodeURIComponent(name)}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  deleteEnvironment: (name: string) =>
    fetch(`/api/world/environment/${encodeURIComponent(name)}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  items: () => fetch("/api/world/items", { cache: "no-store" }).then((r) => json<WorldItem[]>(r)),
  updateItem: (id: string, patch: { name?: string; data?: Record<string, unknown>; status?: string; group?: boolean }) =>
    fetch(`/api/world/items/${id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    }).then((r) => json<WorldItem>(r)),
  todos: () => fetch("/api/world/todos", { cache: "no-store" }).then((r) => json<Todo[]>(r)),
  addTodo: (body: { text: string; priority?: number; due_at?: string; nudge_at?: string; repeat_nudge?: boolean }) =>
    fetch("/api/world/todos", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }).then((r) => json<Todo>(r)),
  updateTodo: (id: string, patch: TodoPatch) =>
    fetch(`/api/world/todos/${id}`, { method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(patch) }).then((r) => json<Todo>(r)),
  decideTodo: (id: string, index: number, proposal?: TodoProposal) =>
    fetch(`/api/world/todos/${id}/decide`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ index, proposal }) }).then((r) => json<Todo>(r)),
  decideTodos: (id: string, picks: { index: number; proposal?: TodoProposal }[]) =>
    fetch(`/api/world/todos/${id}/decide`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ picks }) }).then((r) => json<Todo>(r)),
  clearTodoProposals: (id: string) => fetch(`/api/world/todos/${id}/proposals`, { method: "DELETE" }).then((r) => json<Todo>(r)),
  reorderTodos: (ids: string[]) =>
    fetch("/api/world/todos/order", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ids }) }).then((r) => {
      if (!r.ok) throw new Error(String(r.status));
    }),
  tasks: () => fetch("/api/world/quests", { cache: "no-store" }).then((r) => json<TaskBrief[]>(r)),
  updateQuest: (id: string, patch: QuestPatch) =>
    fetch(`/api/tasks/${id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    }).then((r) => json<{ ok: boolean; status?: string }>(r)),
  assign: (agentId: string, title: string, description: string, repo: string | null, milestones: MilestoneDraft[] = [], waitForMerge = false, timeLimitMinutes: number | null = null, options: QuestOptions = {}) =>
    fetch("/api/tasks", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        title,
        description,
        repo: repo || null,
        kind: repo ? "code" : "doc",
        assigned_agent_id: agentId,
        milestones,
        wait_for_merge: waitForMerge,
        time_limit_minutes: timeLimitMinutes,
        ...options,
      }),
    }).then((r) => json<TaskBrief>(r)),
  addMilestones: (taskId: string, items: MilestoneDraft[]) =>
    fetch(`/api/world/tasks/${taskId}/milestones`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ items }),
    }).then((r) => json<Milestone[]>(r)),
  updateMilestone: (id: string, patch: { title?: string; status?: "pending" | "done" | "skipped" }) =>
    fetch(`/api/world/milestones/${id}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(patch),
    }).then((r) => json<Milestone>(r)),
  deleteMilestone: (id: string) =>
    fetch(`/api/world/milestones/${id}`, { method: "DELETE" }).then((r) => {
      if (!r.ok) throw new Error(`${r.status}`);
    }),
  checkMilestones: (taskId: string) =>
    fetch(`/api/world/tasks/${taskId}/milestones/check`, { method: "POST" }).then((r) => json<{ queued: boolean }>(r)),
};

const ROOFS = ["#b8513a", "#3f6fb0", "#6c4f9e", "#2f7a5a", "#a05a2c", "#9d3f5c"];
const WALLS = ["#f3e6c8", "#e8d4b0", "#f6efe0", "#e9dcc6"];
const AWNINGS: [string, string][] = [
  ["#e85d75", "#fbf3e0"],
  ["#16a39a", "#fbf3e0"],
  ["#e8b64a", "#fbf3e0"],
  ["#6c5ce7", "#fbf3e0"],
  ["#2f80ed", "#fbf3e0"],
];

const GEAR: [RegExp, Accessory[]][] = [
  [/viking|horn/, ["horns"]],
  [/helmet/, ["horns"]],
  [/wizard|staff|hat/, ["wizard"]],
  [/hard ?hat|construction/, ["hardhat"]],
  [/sword|blade|axe/, ["sword"]],
  [/lantern|lamp|torch/, ["lantern"]],
];

export function questFor(agent: Pick<WorldAgent, "gear" | "slug">): Accessory[] {
  const words = text(agent.gear).toLowerCase();
  const found = GEAR.filter(([re]) => re.test(words)).flatMap(([, acc]) => acc);
  if (found.length) return Array.from(new Set(found));
  const presets: Accessory[][] = [["horns", "sword"], ["wizard", "lantern"], ["hardhat", "lantern"], ["horns", "lantern"]];
  return pick(presets, hash(agent.slug), 2);
}

function shopFor(agent: Pick<WorldAgent, "slug">): LokaShopStyle {
  const seed = hash(agent.slug);
  return { roof: pick(ROOFS, seed, 1), wall: pick(WALLS, seed, 4), awning: pick(AWNINGS, seed, 6) };
}

export function residentFor(agent: WorldAgent, isMain = false): Resident {
  const tagline = (agent.persona ?? "").split(/(?<=[.!?])\s/)[0] ?? "";
  return {
    id: agent.id,
    name: agent.name,
    business: isMain ? "Town Hall" : `${agent.name}'s`,
    tagline,
    look: lookFor(agent),
    quest: questFor(agent),
    shop: shopFor(agent),
    trade: tradeFor(agent),
  };
}

const CAUSE_LABELS: Record<string, string> = {
  never_started: "Never got going",
  cli_error: "The CLI hit an error",
  no_report: "Ended without a report",
  timed_out: "Ran out of time",
  stopped: "Stopped",
};

export function causeLabel(cause: string | null | undefined): string {
  return (cause && CAUSE_LABELS[cause]) || "Something went wrong";
}
