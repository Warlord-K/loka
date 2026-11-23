import type { WorldEvent } from "./world";

export type RegionTheme =
  | "meadow"
  | "forest"
  | "docks"
  | "academy"
  | "snow"
  | "desert"
  | "mountain"
  | "swamp"
  | "city"
  | "island";

export type Weather = "clear" | "sunny" | "cloudy" | "rain" | "storm" | "snow" | "fog" | "wind" | "aurora";
export type TimeOfDay = "auto" | "dawn" | "day" | "dusk" | "night";

export interface Ambient {
  weather?: Weather;
  time_of_day?: TimeOfDay;
  critters?: { species: string; count: number }[];
  music?: string;
}

export interface RegionData {
  description?: string;
  ambient?: Ambient;
  [key: string]: unknown;
}

export interface Region {
  id: string;
  owner_user_id: string;
  name: string;
  slug: string;
  theme: RegionTheme;
  seed: number;
  visibility: "private" | "public";
  status: "active" | "archived";
  created_by_agent: string | null;
  data: RegionData;
  created_at: string;
  updated_at: string;
}

export type LokaItemKind =
  | "building"
  | "decoration"
  | "critter"
  | "timer"
  | "list"
  | "portal"
  | "npc"
  | "challenge"
  | "adventure";

export type Surface = "timers" | "schedules" | "sessions" | "workshop" | "terminal" | "devices" | "inbox" | "usage" | "feeds" | "reports";

export type Opens =
  | { kind: "surface"; ref: Surface }
  | { kind: "channel"; ref: string }
  | { kind: "quest"; ref: string }
  | { kind: "list"; ref: string }
  | { kind: "challenge"; ref: string }
  | { kind: "region"; ref: string }
  | { kind: "url"; ref: string };

export interface BuildingData {
  roof: string | null;
  wall: string | null;
  awning: string | null;
  note: string | null;
  opens?: Opens | null;
}

export interface PortalData {
  to_region: string | null;
  label: string;
}

export interface DialogueOption {
  label: string;
  next: string | null;
}

export interface DialogueLine {
  id: string;
  text: string;
  options: DialogueOption[];
}

export interface NpcData {
  persona: string;
  greeting: string | null;
  look: Record<string, unknown>;
  dialogue: DialogueLine[] | null;
  agent_id?: string | null;
}

export interface LokaItem {
  id: string;
  kind: LokaItemKind;
  name: string;
  owner_user_id: string | null;
  created_by_agent: string | null;
  data: Record<string, unknown>;
  status: "active" | "done" | "archived";
  fires_at: string | null;
  created_at: string;
  updated_at: string;
  region_id: string | null;
}

export interface RegionDetail {
  region: Region;
  items: LokaItem[];
}

export interface LokaPreset {
  id: string;
  name: string;
  avatar_emoji: string;
  persona: string;
  tags: string[];
  expertise: string;
  look: Record<string, unknown>;
  gear: Record<string, unknown>;
  visibility: "private" | "public";
}

export interface NpcLine {
  session_id: string;
  said: string | null;
  reply: string | null;
  status: "active" | "completed" | "failed" | string;
  created_at: string;
}

export interface NpcTalk {
  npc_id: string;
  session_id: string;
  run_id: string | null;
  agent_id: string | null;
  status: "queued";
}

export interface NpcVia {
  id: string;
  name: string;
  region_id: string | null;
  region: string | null;
}

export type ChallengeType = "puzzle" | "dsa" | "design" | "quiz" | "reading" | "canvas";
export type Difficulty = "easy" | "medium" | "hard";
export type ChallengeState = "new" | "pending" | "passed" | "failed";
export type AttemptStatus = "pending" | "passed" | "failed" | "error";

export interface ChallengeCard {
  id: string;
  title: string;
  type: ChallengeType;
  skill: string;
  difficulty: Difficulty;
  xp: number;
  minutes: number;
  region_id: string | null;
  adventure_id: string | null;
  adventure_order: number | null;
  task_id: string | null;
  report_id: string | null;
  coach_agent_id: string | null;
  created_by_agent: string | null;
  created_at: string;
  state: ChallengeState;
  attempts: number;
  best_score: number | null;
}

export interface ChallengeTests {
  language: string;
  harness: string;
}

export interface Attempt {
  id: string;
  user_id: string;
  challenge_id: string;
  answer: string;
  status: AttemptStatus;
  score: number | null;
  feedback: string | null;
  xp_awarded: number;
  grader_agent_id: string | null;
  session_id: string | null;
  created_at: string;
  graded_at: string | null;
}

export interface ChallengeDetail {
  card: ChallengeCard;
  prompt: string;
  hints: string[];
  tests: ChallengeTests | null;
  choices: string[];
  reading: { id: string; title: string; kind: string }[];
  coach: { id: string; name: string; slug: string } | null;
  attempts: Attempt[];
}

export interface AdventureCard {
  id: string;
  title: string;
  story: string;
  region_id: string | null;
  folder: string | null;
  skill: string | null;
  coach_agent_id: string | null;
  challenges: string[];
  passed: number;
  total: number;
  completed: boolean;
  created_at: string;
}

export type UnlockKind = "gear" | "decoration" | "house";

export interface Unlock {
  id: string;
  kind: UnlockKind;
  name: string;
  at: string;
}

export interface Progress {
  user_id: string;
  xp: number;
  level: number;
  level_xp: number;
  next_level_xp: number;
  skills: Record<string, number>;
  streak: number;
  best_streak: number;
  streak_alive: boolean;
  last_active_day: string | null;
  unlocks: Unlock[];
  solved: number;
  attempts: number;
  academy_on: boolean;
}

export interface Offer {
  id: string;
  user_id: string;
  agent_id: string | null;
  task_id: string | null;
  status: "open" | "ready" | "expired";
  challenge_id: string | null;
  created_at: string;
  ready_at: string | null;
}

export interface OfferOutcome {
  offer: Offer;
  created: boolean;
}

export interface LokaError {
  error: "not_found" | "busy" | "pending" | "no_quest" | "rate_limited" | "blocked" | "invalid" | "academy_off";
  message: string;
}

export interface LokaEventPayloads {
  "region.created": RegionEventPayload;
  "region.updated": RegionEventPayload;
  "region.archived": RegionEventPayload;
  "item.created": ItemEventPayload;
  "item.updated": ItemEventPayload;
  "item.removed": ItemEventPayload;
  "npc.talked": {
    npc_id: string;
    npc: string;
    session_id: string;
    said: string;
    region_id: string | null;
    owner_user_id: string;
    message_id?: string;
    agent_id?: string;
  };
  "message.created": { message_id: string; author: string; author_kind: "user" | "agent"; body: string; via_npc?: NpcVia };
  "npc.replied": {
    npc_id: string;
    npc: string;
    session_id: string;
    said: string;
    reply: string | null;
    ok: boolean;
    error: string | null;
    owner_user_id: string;
  };
  "challenge.created": {
    challenge_id: string;
    title: string;
    type: ChallengeType;
    skill: string;
    difficulty: Difficulty;
    xp: number;
    minutes: number;
    region_id: string | null;
    owner_user_id: string;
  };
  "challenge.submitted": { attempt_id: string; challenge_id: string; title: string; owner_user_id: string };
  "challenge.graded": {
    attempt_id: string;
    challenge_id: string;
    title: string;
    status: Exclude<AttemptStatus, "pending">;
    score: number | null;
    feedback: string | null;
    xp_awarded: number;
    xp: number;
    level: number;
    level_up: boolean;
    streak: number;
    unlocks: Unlock[];
    owner_user_id: string;
  };
  "academy.unlocked": { unlock: Unlock; owner_user_id: string };
  "academy.offer": {
    offer_id: string;
    task_id: string;
    title: string;
    description: string | null;
    repo: string | null;
    worker: string | null;
    owner_user_id: string;
  };
  "academy.offer_ready": { offer_id: string; challenge_id: string; title: string; minutes: number; owner_user_id: string };
  "adventure.created": { adventure_id: string; title: string; challenges: string[]; region_id: string | null; owner_user_id: string };
  "adventure.completed": { adventure_id: string; title: string; owner_user_id: string };
}

export interface RegionEventPayload {
  region_id: string;
  name: string;
  slug: string;
  theme: RegionTheme;
  visibility: "private" | "public";
  owner_user_id: string;
}

export interface ItemEventPayload {
  item_id: string;
  kind: LokaItemKind;
  name: string;
  owner_user_id: string | null;
  region_id: string | null;
  private: boolean;
}

export type LokaEventType = keyof LokaEventPayloads;

export type LokaEvent<T extends LokaEventType = LokaEventType> = Omit<WorldEvent, "type" | "payload"> & {
  type: T;
  payload: LokaEventPayloads[T];
};

export const LOKA_ROUTES = {
  regions: "/api/world/regions",
  region: (id: string) => `/api/world/regions/${id}`,
  presets: "/api/world/loka/presets",
  npcTalk: (id: string) => `/api/world/npcs/${id}/talk`,
  challenges: "/api/world/challenges",
  challenge: (id: string) => `/api/world/challenges/${id}`,
  attempts: (id: string) => `/api/world/challenges/${id}/attempts`,
  attempt: (id: string) => `/api/world/attempts/${id}`,
  adventures: "/api/world/adventures",
  progress: "/api/world/progress",
  offer: "/api/world/academy/offer",
  academy: "/api/world/academy",
  practiceThis: (reportId: string) => `/api/world/reports/${reportId}/practice`,
} as const;

export const OPEN_ACADEMY = "world:open-academy";

export type AcademyTarget = {
  tab?: "courses" | "progress";
  challengeId?: string | null;
  reportId?: string | null;
  attemptId?: string | null;
  courseId?: string | null;
  newPractice?: boolean;
  request?: number;
};

export function openAcademy(target: AcademyTarget | string | null = null) {
  const detail: AcademyTarget = typeof target === "string" ? { challengeId: target } : (target ?? {});
  window.dispatchEvent(new CustomEvent<AcademyTarget>(OPEN_ACADEMY, { detail }));
}

export type Practiced = { challenge_id: string; report_id: string | null; created: boolean };
