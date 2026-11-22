export type OpenLane = "todo" | "progress";

type Card = { id: string; priority: number };

export function placeOpenTodo<T extends Card>(
  lanes: Record<OpenLane, T[]>,
  card: T,
  lane: OpenLane,
  before: string | null,
): { priority: number; order: string[] } {
  const target = lanes[lane].filter((t) => t.id !== card.id);
  let at = before ? target.findIndex((t) => t.id === before) : -1;
  if (at < 0) at = target.length;
  const above = target[at - 1];
  const below = target[at];
  const reordering = lanes[lane].some((t) => t.id === card.id);
  const priority =
    below || reordering
      ? Math.min(
          Math.max(card.priority, above?.priority ?? card.priority),
          below?.priority ?? Infinity,
        )
      : card.priority;
  const placed = [
    ...target.slice(0, at).map((t) => t.id),
    card.id,
    ...target.slice(at).map((t) => t.id),
  ];
  const other = lanes[lane === "todo" ? "progress" : "todo"]
    .filter((t) => t.id !== card.id)
    .map((t) => t.id);
  return {
    priority,
    order: lane === "todo" ? [...placed, ...other] : [...other, ...placed],
  };
}

export const MOVE_FAILED = "Could not move that to-do, so it is back where it was.";

export function moveFailureNote(saved: { column: boolean; priority: boolean }, columnLabel: string): string {
  if (saved.column) return `Moved to ${columnLabel}, but its place in the list did not save, so it may sit in a different spot.`;
  if (saved.priority) return "Could not save the new order, so the list shows the last order that saved.";
  return MOVE_FAILED;
}
