import type { AdventureCard, ChallengeCard } from "@/lib/loka";
import type { Furniture } from "./interior";
import type { PlaceAction, PlaceRoom } from "./places";
import type { LokaLook } from "./types";

export interface Classroom {
  skill: string;
  name: string;
}

export const CLASSROOMS: Classroom[] = [
  { skill: "algorithms", name: "Algorithms" },
  { skill: "data-structures", name: "Data Structures" },
  { skill: "system-design", name: "System Design" },
  { skill: "distributed-systems", name: "Distributed Systems" },
  { skill: "databases", name: "Databases" },
  { skill: "networking", name: "Networking" },
  { skill: "security", name: "Security" },
  { skill: "frontend", name: "Frontend" },
];

const MAX_ROOMS = 12;
const TINTS = ["#e8b64a", "#7ee0d2", "#ff9ab0", "#9fdcff", "#c39bff", "#9bf08b", "#ff9a5a", "#f2c14e"];

export function skillSlug(skill: string): string {
  return skill.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "general";
}

function titleOf(slug: string): string {
  return slug.split("-").map((w) => w[0].toUpperCase() + w.slice(1)).join(" ");
}

export function classroomsFor(cards: ChallengeCard[]): Classroom[] {
  const out = [...CLASSROOMS];
  for (const c of cards) {
    const slug = skillSlug(c.skill);
    if (!out.some((r) => r.skill === slug)) out.push({ skill: slug, name: titleOf(slug) });
  }
  return out.slice(0, MAX_ROOMS);
}

export interface Mentor {
  id: string;
  name: string;
  skill: string;
  look: LokaLook;
}

export interface CampusData {
  cards: ChallengeCard[];
  adventures: AdventureCard[];
  mentors: Mentor[];
}

function build(id: string, title: string, w: number, h: number, items: [Furniture, PlaceAction | null][], exitTo?: string): PlaceRoom {
  const actions: Record<string, PlaceAction> = {};
  for (const [f, a] of items) if (a && f.label) actions[f.id] = a;
  return { spec: { id, title, w, h, floor: "wood", wall: "#d8cfe6", furniture: items.map(([f]) => f), exitTo }, actions };
}

export function classDoor(skill: string): string {
  return `door:${skill}`;
}

export function hallRoom(data: CampusData): PlaceRoom {
  const rooms = classroomsFor(data.cards);
  const w = Math.max(13, rooms.length * 3 + 3);
  const h = 9;
  const mid = Math.floor(w / 2);
  const count = (skill: string) => data.cards.filter((c) => skillSlug(c.skill) === skill).length;
  const items: [Furniture, PlaceAction | null][] = rooms.map((r, i): [Furniture, PlaceAction] => [
    { id: classDoor(r.skill), kind: "doorway", x: 2 + i * 3, y: 2, label: count(r.skill) ? `${r.name} (${count(r.skill)})` : r.name, tint: TINTS[i % TINTS.length] },
    { kind: "room", room: `class:${r.skill}` },
  ]);
  items.push(
    [{ id: "reception", kind: "counter", x: mid - 1, y: 5, label: "Reception: all challenges and progress", colors: ["#fbf3e0", "#e8b64a", "#7ee0d2", "#c39bff"] }, { kind: "academy" }],
    [{ id: "plant-a", kind: "plant", x: 1, y: 7 }, null],
    [{ id: "plant-b", kind: "plant", x: w - 2, y: 7 }, null],
  );
  return build("campus:hall", "University", w, h, items);
}

const DESKS: [number, number][] = [[2, 5], [5, 5], [8, 5], [2, 7], [8, 7], [10, 5]];

export function classRoom(skill: string, data: CampusData): PlaceRoom {
  const room = classroomsFor(data.cards).find((r) => r.skill === skill) ?? { skill, name: titleOf(skill) };
  const cards = data.cards.filter((c) => skillSlug(c.skill) === skill);
  const ids = new Set(cards.map((c) => c.id));
  const adventures = data.adventures.filter((a) => a.challenges.some((id) => ids.has(id)));
  const items: [Furniture, PlaceAction | null][] = [
    [{ id: "board", kind: "chart", x: 5, y: 2, label: `${room.name} board` }, { kind: "academy" }],
    [{ id: "lamp", kind: "lamp", x: 11, y: 7 }, null],
    [{ id: "plant", kind: "plant", x: 1, y: 8 }, null],
  ];
  if (adventures.length) items.push([{ id: "adventures", kind: "shelf", x: 9, y: 2, label: `Adventures (${adventures.length})`, colors: adventures.map((_, i) => TINTS[i % TINTS.length]) }, { kind: "academy" }]);
  cards.slice(0, DESKS.length).forEach((c, i) => {
    const [x, y] = DESKS[i];
    items.push([{ id: `desk:${c.id}`, kind: "desk", x, y, label: `${c.title}${c.state === "passed" ? " (passed)" : ""}` }, { kind: "academy", challengeId: c.id }]);
  });
  if (!cards.length) items.push([{ id: "desk:none", kind: "desk", x: 5, y: 5, label: "No challenges here yet" }, { kind: "academy" }]);
  data.mentors
    .filter((m) => m.skill === skill)
    .slice(0, 2)
    .forEach((m, i) => items.push([{ id: `mentor:${m.id}`, kind: "mentor", x: i ? 10 : 2, y: 3, label: `Talk to ${m.name}`, look: m.look }, { kind: "npc", id: m.id }]));
  return build(`campus:class:${skill}`, `${room.name} classroom`, 13, 10, items, "hall");
}

export function campusRoom(sub: string | undefined, data: CampusData): PlaceRoom {
  return sub?.startsWith("class:") ? classRoom(sub.slice(6), data) : hallRoom(data);
}
