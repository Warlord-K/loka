export type RoomMessageKind = "chat" | "question" | "answer" | "decision" | "needs_you" | "assumed";
export type DecisionStatus = "open" | "answered" | "passed_up" | "assumed" | "parked";
export type QuestionAbout = "fact" | "choice" | "taste" | "credentials" | "money" | "irreversible";
export type Asking = "off" | "ask_agent" | "crew";
export type MemberKind = "agent" | "person" | "session";
export type MemberRole = "owner" | "lead" | "helper" | "guest";

export interface Party {
  kind: "agent" | "person" | "session" | "loka";
  id: string | null;
  name: string;
}

export interface RoomMember {
  quest_id?: string;
  member_kind: MemberKind;
  member_id: string;
  role: MemberRole;
  name: string;
  added_by?: string | null;
  added_by_kind?: "agent" | "person" | "loka";
  added_at?: string | null;
  default?: boolean;
  cli?: string | null;
  model?: string | null;
}

export interface Runtime {
  cli: string;
  model: string;
}

export type MixSlot = { cli?: string | null; model?: string | null } | null;

export function slotEmpty(slot: MixSlot | undefined): boolean {
  return !slot || (!slot.cli?.trim() && !slot.model?.trim());
}

export function slotRuntime(slot: MixSlot | undefined, lead: Runtime): Runtime | null {
  if (slotEmpty(slot)) return null;
  const cli = slot?.cli?.trim() || lead.cli;
  const model = slot?.model?.trim() || (cli === lead.cli ? lead.model : "");
  return { cli, model };
}

export function cleanVariants(variants: MixSlot[], size: number): MixSlot[] {
  const slots = Array.from({ length: size }, (_, i): MixSlot => {
    const cli = variants[i]?.cli?.trim() ?? "";
    const model = variants[i]?.model?.trim() ?? "";
    if (!cli && !model) return null;
    return cli ? { cli, model } : { model };
  });
  return slots.some(Boolean) ? slots : [];
}

export function mixProblem(variants: MixSlot[], size: number): string | null {
  for (let i = 0; i < size; i++) {
    const slot = variants[i];
    if (slot?.cli?.trim() && !slot.model?.trim()) return `Pick a model for helper ${i + 1}, or set it back to Loka picks.`;
  }
  return null;
}

export type MixSource = "quest" | "agent" | "loka";

export function resolveMix(size: number, lead: Runtime, quest: MixSlot[], agent: MixSlot[], defaults: Runtime[]): { runtime: Runtime | null; source: MixSource }[] {
  return Array.from({ length: size }, (_, i) => {
    const own = slotRuntime(quest[i], lead);
    if (own) return { runtime: own, source: "quest" as const };
    const standing = slotRuntime(agent[i], lead);
    if (standing) return { runtime: standing, source: "agent" as const };
    return { runtime: defaults[i] ?? null, source: "loka" as const };
  });
}

export function decisionTarget<M extends BoardMessage>(thread: Pick<Thread<M>, "question" | "answer">): string {
  return thread.answer?.id ?? thread.question.id;
}

export function questionRecipient(
  question: Pick<BoardMessage, "agent_id">,
  agents: Map<string, { name: string }>,
  members: RoomMember[] | null | undefined,
  fallback: string,
): string {
  return agents.get(question.agent_id)?.name ?? members?.find((member) => member.member_kind === "agent" && member.member_id === question.agent_id)?.name ?? fallback;
}

export function roomMentionHandles(bases: string[]): string[] {
  const used = new Map<string, number>();
  return bases.map((base) => {
    const count = (used.get(base) ?? 0) + 1;
    used.set(base, count);
    return count === 1 ? base : `${base}${count}`;
  });
}

export interface DecisionSummary {
  open: number;
  answered_by_agent: number;
  answered_by_lead?: number | null;
  answered_by_person: number;
  passed_up: number;
  assumed: number;
  parked: number;
  avg_wait_ms?: number | null;
  tokens?: number | null;
}

export interface RoomAsking {
  mode: Asking;
  crew_size?: number | null;
  per_run?: number;
  per_quest?: number;
  asked?: number;
}

export interface QuestionMeta {
  post_id?: string | null;
  about?: QuestionAbout | null;
  assumption?: string | null;
  part_id?: string | null;
  refs?: string[];
}

export interface BoardMessage {
  id: string;
  agent_id: string;
  author_kind: "user" | "agent" | "system";
  author_user_id: string | null;
  author_agent_id: string | null;
  body: string;
  created_at: string;
  kind?: RoomMessageKind | null;
  decision_status?: DecisionStatus | null;
  answered_by?: Party | null;
  routes_to?: Party | null;
  asked_by_session?: string | null;
  meta: {
    task_id?: string;
    answers?: string;
    answers_question?: string;
    refs?: string[];
    question?: QuestionMeta;
    suggestion?: string | null;
    choices?: string[];
    why?: string | null;
    clarification?: { task_id?: string; session_id?: string } | null;
    early_answer?: { text: string; by: Party | null } | null;
  } | null;
}

export interface BoardSession {
  id: string;
  agent_id: string;
  task_id?: string | null;
  role: string;
  status: string;
  running: boolean;
  backend?: string;
  model?: string;
  cost_usd: number;
  plan_state?: string | null;
}

export interface BoardAgent {
  id: string;
  name: string;
  cli: string;
  model: string;
  status: string;
  paused_reason?: string | null;
  archived_at?: string | null;
}

export interface Thread<M extends BoardMessage = BoardMessage> {
  group?: Thread<M>[];
  question: M;
  status: DecisionStatus;
  about: QuestionAbout | null;
  answer: M | null;
  answerText: string | null;
  by: Party | null;
  card: M | null;
}

export interface Board<M extends BoardMessage = BoardMessage> {
  threads: Thread<M>[];
  all: Thread<M>[];
  needsYou: M[];
  decisions: Thread<M>[];
  folded: Set<string>;
  focusAliases: Map<string, string>;
  summary: DecisionSummary;
}

const SETTLING: RoomMessageKind[] = ["answer", "decision", "assumed"];
export const YOURS: QuestionAbout[] = ["taste", "credentials", "money", "irreversible"];

export function kindOf(message: Pick<BoardMessage, "kind">): RoomMessageKind {
  return message.kind ?? "chat";
}

function answeredQuestion(message: BoardMessage): string | null {
  return message.meta?.answers_question ?? null;
}

function newer<M extends BoardMessage>(a: M | null, b: M): M {
  return !a || b.created_at >= a.created_at ? b : a;
}

export function threadStatus(question: BoardMessage, answer: BoardMessage | null, card: BoardMessage | null): DecisionStatus {
  const stored = question.decision_status ?? "open";
  if (stored === "parked") return "parked";
  if (answer) return kindOf(answer) === "assumed" ? "assumed" : "answered";
  if (stored === "answered" || stored === "assumed") return stored;
  if (card || stored === "passed_up") return "passed_up";
  return "open";
}

export function emptySummary(): DecisionSummary {
  return { open: 0, answered_by_agent: 0, answered_by_person: 0, passed_up: 0, assumed: 0, parked: 0 };
}

function decidedBy(thread: Pick<Thread, "answer" | "by">): "person" | "agent" {
  if (thread.by?.kind === "person") return "person";
  if (!thread.by && thread.answer?.author_kind === "user") return "person";
  return "agent";
}

export function buildBoard<M extends BoardMessage>(messages: M[], options: { status?: string | null; clarifying?: Set<string>; lead?: string | null; needsYouMessages?: M[] } = {}): Board<M> {
  const questStatus = options.status;
  const source = options.needsYouMessages === undefined ? messages : mergeRoomMessages(options.needsYouMessages, messages);
  const pendingQuestions = options.needsYouMessages === undefined
    ? null
    : new Set(
        options.needsYouMessages
          .flatMap((message) => kindOf(message) === "question" ? [message.id] : [answeredQuestion(message)])
          .filter((id): id is string => !!id),
      );
  const pendingCards = options.needsYouMessages === undefined
    ? null
    : new Map(options.needsYouMessages.map((message) => [answeredQuestion(message), message] as const).filter((entry): entry is [string, M] => !!entry[0] && kindOf(entry[1]) === "needs_you"));
  const questions = new Map<string, M>();
  for (const m of source) if (kindOf(m) === "question") questions.set(m.id, m);
  const answers = new Map<string, M>();
  const cards = new Map<string, M>();
  const focusAliases = new Map<string, string>();
  const folded = new Set<string>();
  for (const m of source) {
    const target = answeredQuestion(m) ?? (m.author_kind === "user" && m.meta?.answers && questions.has(m.meta.answers) ? m.meta.answers : null);
    if (!target || !questions.has(target)) continue;
    focusAliases.set(m.id, target);
    const kind = kindOf(m);
    if (kind === "needs_you") {
      cards.set(target, newer(cards.get(target) ?? null, m));
      folded.add(m.id);
    } else if (SETTLING.includes(kind) || m.author_kind === "user") {
      answers.set(target, newer(answers.get(target) ?? null, m));
      folded.add(m.id);
    }
  }
  const replies = new Map<string, M>();
  for (const m of source) if (m.author_kind === "user" && m.meta?.answers) replies.set(m.meta.answers, newer(replies.get(m.meta.answers) ?? null, m));
  const closed = questStatus === "done" || questStatus === "cancelled";
  const summary = emptySummary();
  const threads: Thread<M>[] = [];
  for (const question of questions.values()) {
    const card = pendingCards ? pendingCards.get(question.id) ?? null : cards.get(question.id) ?? null;
    const reply = card ? replies.get(card.id) ?? null : null;
    if (reply) folded.add(reply.id);
    const answer = answers.get(question.id) ?? reply;
    const status = threadStatus(question, answer, card);
    const early = question.meta?.early_answer ?? null;
    const by = answer?.answered_by ?? question.answered_by ?? early?.by ?? null;
    const thread: Thread<M> = {
      question,
      status,
      about: question.meta?.question?.about ?? null,
      answer,
      answerText: answer?.body ?? early?.text ?? null,
      by,
      card,
    };
    threads.push(thread);
    if (status === "open") summary.open++;
    else if (status === "passed_up") {
      if (!pendingQuestions || pendingQuestions.has(question.id)) summary.passed_up++;
    }
    else if (status === "assumed") summary.assumed++;
    else if (status === "parked") summary.parked++;
    else if (decidedBy(thread) === "person") summary.answered_by_person++;
    else {
      summary.answered_by_agent++;
      if (options.lead) {
        const decider = thread.by?.kind === "agent" ? thread.by.id : !thread.by ? question.routes_to?.id ?? null : null;
        if (decider === options.lead) summary.answered_by_lead = (summary.answered_by_lead ?? 0) + 1;
      }
    }
  }
  threads.sort((a, b) => (a.question.created_at < b.question.created_at ? -1 : a.question.created_at > b.question.created_at ? 1 : 0));
  const needsYou: M[] = [];
  if (!closed) {
    for (const thread of threads) if (thread.status === "passed_up" && thread.card && (!pendingQuestions || pendingQuestions.has(thread.question.id))) needsYou.push(thread.card);
    if (options.needsYouMessages !== undefined) {
      const snapshotIds = new Set(options.needsYouMessages.map((message) => message.id));
      for (const m of source) {
        const session = m.meta?.clarification?.session_id;
        const current = !session || !options.clarifying || options.clarifying.has(session);
        const legacy = !answeredQuestion(m) && kindOf(m) !== "question" && (kindOf(m) === "needs_you" || !!m.meta?.clarification);
        if (snapshotIds.has(m.id) && legacy && current && m.author_kind !== "user" && !replies.has(m.id)) needsYou.push(m);
      }
    } else {
      const newestCard = new Map<string, M>();
      for (const m of source) {
        const session = m.meta?.clarification?.session_id;
        if (session && m.author_kind !== "user") newestCard.set(session, newer(newestCard.get(session) ?? null, m));
      }
      for (const m of source) {
        const loose = kindOf(m) === "needs_you" && !answeredQuestion(m);
        const session = m.meta?.clarification?.session_id;
        const clarify = !!session && !!options.clarifying?.has(session) && newestCard.get(session)?.id === m.id;
        if ((loose || clarify) && m.author_kind !== "user" && !replies.has(m.id)) needsYou.push(m);
      }
    }
    needsYou.sort((a, b) => (a.created_at < b.created_at ? -1 : a.created_at > b.created_at ? 1 : 0));
  }
  const decisions = threads.filter((t) => t.status === "answered" || t.status === "assumed" || t.status === "parked");
  const posts = new Map<string, Thread<M>>();
  const shown: Thread<M>[] = [];
  for (const thread of threads) {
    const post = thread.question.meta?.question?.post_id;
    const lead = post ? posts.get(post) : undefined;
    if (lead) {
      lead.group!.push(thread);
      folded.add(thread.question.id);
      continue;
    }
    if (post) {
      thread.group = [thread];
      posts.set(post, thread);
    }
    shown.push(thread);
  }
  return { threads: shown, all: threads, needsYou, decisions, folded, focusAliases, summary };
}

export function summaryText(summary: DecisionSummary | null | undefined, lead: string | null | undefined): string {
  if (!summary) return "";
  const parts: string[] = [];
  const byLead = summary.answered_by_lead ?? null;
  const byOthers = byLead === null ? summary.answered_by_agent : summary.answered_by_agent - byLead;
  if (byLead && lead) parts.push(`${byLead} decided by ${lead}`);
  else if (byLead) parts.push(`${byLead} decided by the lead`);
  if (byOthers > 0) parts.push(`${byOthers} decided by ${byLead === null ? (byOthers === 1 ? "an agent" : "agents") : byOthers === 1 ? "another agent" : "other agents"}`);
  if (summary.answered_by_person > 0) parts.push(`${summary.answered_by_person} decided by you`);
  if (summary.assumed > 0) parts.push(`${summary.assumed} assumed`);
  if (summary.parked > 0) parts.push(`${summary.parked} parked as a to-do`);
  if (summary.open > 0) parts.push(`${summary.open} ${summary.open === 1 ? "question" : "questions"} open`);
  if (summary.passed_up > 0) parts.push(`${summary.passed_up} ${summary.passed_up === 1 ? "needs" : "need"} you`);
  return parts.join(", ");
}

export const ABOUT_LABEL: Record<QuestionAbout, string> = {
  fact: "A fact",
  choice: "A choice",
  taste: "Taste",
  credentials: "A key or sign-in",
  money: "Money",
  irreversible: "Can't be undone",
};

export function whyLabel(about: string | null | undefined): string | null {
  if (!about) return null;
  if (about in ABOUT_LABEL) return ABOUT_LABEL[about as QuestionAbout];
  if (about === "other") return "Yours to decide";
  return null;
}

export const ROLE_NAME: Record<MemberRole, string> = { owner: "Owner", lead: "Lead", helper: "Helper", guest: "Guest" };

export type CrewState = "working" | "deciding" | "waiting" | "asking" | "paused" | "needs_you" | "idle" | "done" | "away" | "error";

export interface CrewEntry {
  key: string;
  kind: MemberKind;
  role: MemberRole;
  id: string;
  name: string;
  agentId: string | null;
  sessionId: string | null;
  runtime: Runtime | null;
  state: CrewState;
  detail: string;
  removable: boolean;
}

const STATE_TEXT: Record<CrewState, string> = {
  working: "working",
  deciding: "deciding",
  waiting: "waiting for an answer",
  asking: "waiting for your answers",
  paused: "paused",
  needs_you: "needs you",
  idle: "idle",
  done: "done",
  away: "away",
  error: "has a problem",
};

export const LIVE_STATES: CrewState[] = ["working", "deciding"];

function agentState(agent: BoardAgent | undefined, deciding: boolean): CrewState {
  if (!agent) return "idle";
  if (agent.paused_reason || agent.archived_at) return "paused";
  if (deciding) return "deciding";
  if (agent.status === "thinking") return "working";
  if (agent.status === "waiting") return "needs_you";
  if (agent.status === "error") return "error";
  if (agent.status === "away") return "away";
  return "idle";
}

function sessionState<S extends BoardSession>(session: S | undefined, agent: BoardAgent | undefined, asking: boolean, awaitsPlan: (session: S) => boolean): CrewState {
  if (agent?.paused_reason || agent?.archived_at) return "paused";
  if (!session) return "idle";
  if (asking) return "waiting";
  if (session.plan_state === "clarifying") return "asking";
  if (awaitsPlan(session)) return "needs_you";
  if (session.running) return "working";
  if (session.status === "archived" || session.status === "completed" || session.status === "done") return "done";
  return "idle";
}

export function crewOf<S extends BoardSession, M extends BoardMessage>(input: {
  members: RoomMember[] | null | undefined;
  sessions: S[];
  agents: Map<string, BoardAgent>;
  lead: string | null | undefined;
  threads: Thread<M>[];
  sessionLabel: (session: S) => string;
  awaitsPlan: (session: S) => boolean;
}): CrewEntry[] {
  const { sessions, agents, lead, threads, sessionLabel, awaitsPlan } = input;
  const open = threads.filter((t) => t.status === "open");
  const asking = new Set(open.map((t) => t.question.asked_by_session).filter((id): id is string => !!id));
  const deciding = new Set(open.map((t) => (t.question.routes_to?.kind === "agent" ? t.question.routes_to.id : null)).filter((id): id is string => !!id));
  const bySession = new Map(sessions.map((s) => [s.id, s]));
  const members: RoomMember[] =
    input.members ??
    [
      ...(lead ? [{ member_kind: "agent" as const, member_id: lead, role: "lead" as const, name: agents.get(lead)?.name ?? "Agent", default: true }] : []),
      ...sessions.filter((s) => s.role !== "main").map((s) => ({ member_kind: "session" as const, member_id: s.id, role: "helper" as const, name: agents.get(s.agent_id)?.name ?? "Agent", default: true })),
    ];
  const order: Record<MemberRole, number> = { lead: 0, helper: 1, guest: 2, owner: 3 };
  return members
    .map((m, i) => ({ m, i }))
    .sort((a, b) => order[a.m.role] - order[b.m.role] || a.i - b.i)
    .map(({ m }): CrewEntry => {
      if (m.member_kind === "session") {
        const session = bySession.get(m.member_id);
        const agent = session ? agents.get(session.agent_id) : undefined;
        const state = sessionState(session, agent, asking.has(m.member_id), awaitsPlan);
        return {
          key: `session:${m.member_id}`,
          kind: "session",
          role: m.role,
          id: m.member_id,
          name: agent?.name ?? m.name,
          agentId: session?.agent_id ?? null,
          sessionId: m.member_id,
          runtime: m.cli ? { cli: m.cli, model: m.model ?? "" } : session?.backend ? { cli: session.backend, model: session.model ?? "" } : agent ? { cli: agent.cli, model: agent.model } : null,
          state,
          detail: session ? `${sessionLabel(session)} · ${STATE_TEXT[state]}` : STATE_TEXT[state],
          removable: false,
        };
      }
      if (m.member_kind === "agent") {
        const agent = agents.get(m.member_id);
        const state = agentState(agent, deciding.has(m.member_id));
        return {
          key: `agent:${m.member_id}`,
          kind: "agent",
          role: m.role,
          id: m.member_id,
          name: agent?.name ?? m.name,
          agentId: m.member_id,
          sessionId: null,
          runtime: m.cli ? { cli: m.cli, model: m.model ?? "" } : agent ? { cli: agent.cli, model: agent.model } : null,
          state,
          detail: `${ROLE_NAME[m.role]} · ${STATE_TEXT[state]}`,
          removable: !m.default && m.role === "guest",
        };
      }
      return {
        key: `person:${m.member_id}`,
        kind: "person",
        role: m.role,
        id: m.member_id,
        name: m.name,
        agentId: null,
        sessionId: null,
        runtime: null,
        state: "idle",
        detail: ROLE_NAME[m.role],
        removable: !m.default && m.role === "guest",
      };
    });
}

export function crewStateText(state: CrewState): string {
  return STATE_TEXT[state];
}

export interface CrewStats {
  helpers: number;
  working: number;
  open: number;
  decided: number;
  needsYou: number;
  costUsd: number;
}

export function crewStats(crew: CrewEntry[], board: Pick<Board, "summary" | "needsYou">, sessions: BoardSession[], decisions?: DecisionSummary | null): CrewStats {
  const helpers = crew.filter((c) => c.kind === "session" || c.role === "helper");
  const summary = decisions ?? board.summary;
  return {
    helpers: helpers.length,
    working: helpers.filter((c) => LIVE_STATES.includes(c.state)).length,
    open: summary.open,
    decided: summary.answered_by_agent + summary.answered_by_person,
    needsYou: board.needsYou.length,
    costUsd: sessions.reduce((n, s) => n + (s.cost_usd || 0), 0),
  };
}

export function crewCardSpec(stats: CrewStats, cost: string) {
  const settled = stats.decided + stats.open + stats.needsYou;
  const share = settled ? Math.round((stats.decided / settled) * 100) : 0;
  return {
    root: "card",
    elements: {
      card: { type: "Stack", props: { direction: "column", gap: "md" }, children: ["row", "bar"] },
      row: { type: "Stack", props: { direction: "row", gap: "lg" }, children: ["helpers", "open", "decided", "you", "cost"] },
      helpers: { type: "Metric", props: { label: "Helpers", value: stats.helpers, hint: stats.working ? `${stats.working} working` : null } },
      open: { type: "Metric", props: { label: "Open questions", value: stats.open, hint: null } },
      decided: { type: "Metric", props: { label: "Decided", value: stats.decided, hint: null } },
      you: { type: "Metric", props: { label: "Needs you", value: stats.needsYou, hint: null } },
      cost: { type: "Metric", props: { label: "Cost so far", value: cost, hint: null } },
      bar: { type: "Progress", props: { label: settled ? `${stats.decided} of ${settled} questions decided` : "No questions yet", value: share } },
    },
  };
}

export interface RoomEvent {
  type: string;
  task_id?: string | null;
  agent_id?: string | null;
  session_id?: string | null;
  created_at: string;
  payload?: Record<string, unknown> | null;
}

export interface RoomScope {
  canonicalTaskId: string;
  taskIds: Set<string>;
  sessionIds: Set<string>;
}

export function roomScope(
  taskId: string,
  canonicalTaskId: string | null | undefined,
  members: RoomMember[] | null | undefined,
  partIds: string[],
  sessions: Pick<BoardSession, "id" | "task_id">[],
): RoomScope {
  const canonical = canonicalTaskId || members?.find((member) => member.quest_id)?.quest_id || taskId;
  const taskIds = new Set([taskId, canonical, ...partIds]);
  for (const member of members ?? []) if (member.quest_id) taskIds.add(member.quest_id);
  for (const session of sessions) if (session.task_id) taskIds.add(session.task_id);
  return { canonicalTaskId: canonical, taskIds, sessionIds: new Set(sessions.map((session) => session.id)) };
}

export function roomEventMatches(scope: RoomScope, event: Pick<RoomEvent, "task_id" | "session_id">): boolean {
  return (!!event.task_id && scope.taskIds.has(event.task_id)) || (!!event.session_id && scope.sessionIds.has(event.session_id));
}

export interface RoomLive<M extends BoardMessage = BoardMessage> {
  messages: M[];
  members: RoomMember[] | null | undefined;
}

export const ROOM_EVENTS = ["session.asked", "session.answered", "room.decision", "room.member_added", "room.member_removed"];

function text(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function party(v: unknown): Party | null {
  if (!v || typeof v !== "object") return null;
  const p = v as Record<string, unknown>;
  const kind = text(p.kind);
  if (kind !== "agent" && kind !== "person" && kind !== "session" && kind !== "loka") return null;
  return { kind, id: text(p.id) || null, name: text(p.name) };
}

function status(v: unknown): DecisionStatus | null {
  const s = text(v);
  if (s === "changed") return "answered";
  return s === "open" || s === "answered" || s === "passed_up" || s === "assumed" || s === "parked" ? s : null;
}

function about(v: unknown): QuestionAbout | null {
  const s = text(v);
  return s in ABOUT_LABEL ? (s as QuestionAbout) : null;
}

export function roomEventSettlement(event: RoomEvent, taskId: string): { questionId: string; status: DecisionStatus } | null {
  const payload = event.payload ?? {};
  if ((event.task_id ?? text(payload.task_id)) !== taskId) return null;
  const questionId = text(payload.question_id);
  if (!questionId) return null;
  const next = event.type === "session.answered" ? status(payload.status) ?? "answered" : event.type === "room.decision" ? status(payload.status) : null;
  if (next === "answered" || next === "assumed" || next === "parked") return { questionId, status: next };
  return null;
}

export function applyRoomEvent<M extends BoardMessage>(live: RoomLive<M>, event: RoomEvent, taskId: string): RoomLive<M> {
  const p = event.payload ?? {};
  if ((event.task_id ?? text(p.task_id)) !== taskId) return live;
  const questionId = text(p.question_id);
  const patch = (change: (q: M) => M): RoomLive<M> => {
    let hit = false;
    const messages = live.messages.map((m) => {
      if (m.id !== questionId) return m;
      hit = true;
      return change(m);
    });
    return hit ? { ...live, messages } : live;
  };
  switch (event.type) {
    case "session.asked": {
      if (!questionId || live.messages.some((m) => m.id === questionId)) return live;
      const to = party(p.to);
      const question = {
        id: questionId,
        agent_id: (to?.kind === "agent" && to.id) || event.agent_id || "",
        author_kind: "agent",
        author_user_id: null,
        author_agent_id: event.agent_id ?? null,
        body: text(p.question),
        created_at: event.created_at,
        kind: "question",
        decision_status: "open",
        answered_by: null,
        routes_to: to,
        asked_by_session: text(p.session_id) || null,
        meta: { task_id: taskId, question: { about: about(p.about), post_id: text(p.post_id) || null } },
      } as M;
      return { ...live, messages: [...live.messages, question] };
    }
    case "session.answered": {
      const next = status(p.status) ?? "answered";
      const by = party(p.by);
      const answer = text(p.answer);
      return patch((q) => ({
        ...q,
        decision_status: next,
        answered_by: by ?? q.answered_by ?? null,
        meta: { ...(q.meta ?? {}), early_answer: answer ? { text: answer, by } : q.meta?.early_answer ?? null },
      }));
    }
    case "room.decision": {
      const next = status(p.status);
      if (!next) return live;
      const by = party(p.by);
      return patch((q) => ({ ...q, decision_status: next, answered_by: by ?? q.answered_by ?? null }));
    }
    case "room.member_added": {
      if (!live.members) return live;
      const kind = text(p.member_kind);
      const id = text(p.member_id);
      if ((kind !== "agent" && kind !== "person" && kind !== "session") || !id) return live;
      const role = text(p.role);
      const member: RoomMember = {
        member_kind: kind,
        member_id: id,
        role: role === "owner" || role === "lead" || role === "helper" ? role : "guest",
        name: text(p.name),
        added_at: event.created_at,
      };
      const rest = live.members.filter((m) => !(m.member_kind === kind && m.member_id === id));
      return { ...live, members: [...rest, member] };
    }
    case "room.member_removed": {
      if (!live.members) return live;
      const kind = text(p.member_kind);
      const id = text(p.member_id);
      const members = live.members.filter((m) => !(m.member_kind === kind && m.member_id === id) || m.default);
      return members.length === live.members.length ? live : { ...live, members };
    }
    default:
      return live;
  }
}

export function mergeRoomMessages<M extends BoardMessage>(prev: M[], fresh: M[]): M[] {
  if (!fresh.length) return prev;
  const byId = new Map(fresh.map((m) => [m.id, m]));
  const kept = prev.map((m) => {
    const next = byId.get(m.id);
    if (!next) return m;
    const staleOpen = (next.decision_status ?? "open") === "open" && (m.decision_status ?? "open") !== "open";
    const earlyAnswer = m.meta?.early_answer && !next.meta?.early_answer ? m.meta.early_answer : null;
    if (staleOpen || earlyAnswer)
      return {
        ...next,
        decision_status: staleOpen ? m.decision_status : next.decision_status,
        answered_by: staleOpen ? m.answered_by : next.answered_by,
        meta: earlyAnswer ? { ...(next.meta ?? {}), early_answer: earlyAnswer } : next.meta,
      };
    return next;
  });
  const known = new Set(prev.map((m) => m.id));
  return [...kept, ...fresh.filter((m) => !known.has(m.id))];
}

export function applyRoomSettlements<M extends BoardMessage>(messages: M[], settlements: ReadonlyMap<string, DecisionStatus>): M[] {
  if (!settlements.size) return messages;
  let changed = false;
  const next = messages.map((message) => {
    const status = kindOf(message) === "question" ? settlements.get(message.id) : undefined;
    if (!status || message.decision_status === status) return message;
    const stored = message.decision_status ?? "open";
    if (stored !== "open" && stored !== "passed_up") return message;
    changed = true;
    return { ...message, decision_status: status };
  });
  return changed ? next : messages;
}

export function acknowledgeRoomSettlements<M extends BoardMessage>(settlements: Map<string, DecisionStatus>, messages: M[], snapshot: M[] | undefined): void {
  for (const [questionId, status] of settlements) {
    const questions = [...messages, ...(snapshot ?? [])].filter((message) => kindOf(message) === "question" && message.id === questionId);
    const acknowledged = questions.some((message) => message.decision_status === status);
    const terminal = questions.some((message) => {
      const stored = message.decision_status ?? "open";
      return stored !== "open" && stored !== "passed_up";
    });
    const removed = snapshot !== undefined && !snapshot.some((message) => kindOf(message) === "question" && message.id === questionId);
    if (acknowledged || terminal || removed) settlements.delete(questionId);
  }
}

export function cardChoices(meta: BoardMessage["meta"]): string[] {
  const list = (meta?.choices ?? []).filter((c): c is string => typeof c === "string" && c.trim().length > 0);
  const suggestion = meta?.suggestion?.trim();
  return suggestion ? [suggestion, ...list.filter((c) => c.trim() !== suggestion)] : list;
}

export interface BoardQuest {
  id: string;
  status: string;
  asking?: Asking | null;
  decisions?: DecisionSummary | null;
}

export function boardQuests<Q extends BoardQuest>(quests: Q[], limit = 3): Q[] {
  const open = quests.filter((q) => q.status !== "done" && q.status !== "cancelled");
  const weight = (q: Q) => {
    const d = q.decisions;
    const settled = d ? d.answered_by_agent + d.answered_by_person + d.assumed + d.parked : 0;
    return (d?.passed_up ? 8 : 0) + (d?.open ? 4 : 0) + (q.asking === "crew" ? 2 : 0) + (settled ? 1 : 0);
  };
  const ranked = open
    .map((q, i) => ({ q, i, w: weight(q) }))
    .filter((x) => x.w > 0)
    .sort((a, b) => b.w - a.w || a.i - b.i)
    .map((x) => x.q);
  if (ranked.length) return ranked.slice(0, limit);
  const working = open.find((q) => q.status === "in_progress");
  return working ? [working] : [];
}

export function threadOf<M extends BoardMessage>(board: Pick<Board<M>, "threads" | "focusAliases">, id: string | null | undefined): Thread<M> | null {
  if (!id) return null;
  const target = board.focusAliases.get(id) ?? id;
  return board.threads.find((t) => (t.group ?? [t]).some((g) => g.question.id === target || g.answer?.id === id || g.card?.id === id)) ?? null;
}

export function roomAsk(event: Pick<RoomEvent, "type" | "task_id" | "payload">): { taskId: string; focus: string | null } | null {
  if (event.type !== "attention.requested") return null;
  const p = event.payload ?? {};
  const taskId = event.task_id || text(p.task_id);
  if (!taskId || !text(p.question_id)) return null;
  return { taskId, focus: text(p.message_id) || null };
}

export function openRoomAsk(ask: { taskId: string; focus: string | null }, agentId: string | null) {
  window.dispatchEvent(new CustomEvent("world:open-quest", { detail: { taskId: ask.taskId, agentId, focus: ask.focus } }));
}

export function pairQuote(thread: Pick<Thread, "question" | "answerText" | "by" | "status">, asker: string): string {
  const who = thread.by?.name || "the agent";
  const lines = [`${asker} asked: ${thread.question.body.trim()}`];
  if (thread.answerText) lines.push(`${thread.status === "assumed" ? "Assumed" : `${who} decided`}: ${thread.answerText.trim()}`);
  return lines.join("\n");
}
