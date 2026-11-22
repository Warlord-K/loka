export type MusicBag = { left: string[]; played: string[]; last: string | null };

export type Random = () => number;

export const EMPTY_BAG: MusicBag = { left: [], played: [], last: null };

const VIDEO_ID = /^[A-Za-z0-9_-]{11}$/;
const MAX_STORED = 500;

function unique(ids: string[], keep: (id: string) => boolean): string[] {
  const seen = new Set<string>();
  return ids.filter((id) => {
    if (seen.has(id) || !keep(id)) return false;
    seen.add(id);
    return true;
  });
}

function shuffled(ids: string[], random: Random): string[] {
  const order = [...ids];
  for (let i = order.length - 1; i > 0; i--) {
    const j = Math.floor(random() * (i + 1));
    [order[i], order[j]] = [order[j], order[i]];
  }
  return order;
}

export function freshLap(ids: string[], last: string | null, random: Random): string[] {
  const order = shuffled(unique(ids, () => true), random);
  if (order.length > 1 && order[0] === last) {
    const j = 1 + Math.floor(random() * (order.length - 1));
    [order[0], order[j]] = [order[j], order[0]];
  }
  return order;
}

export function syncBag(bag: MusicBag, ids: string[], random: Random): MusicBag {
  const known = new Set(ids);
  const left = unique(bag.left, (id) => known.has(id));
  const leftSet = new Set(left);
  const played = unique(bag.played, (id) => known.has(id) && !leftSet.has(id));
  if (!left.length && !played.length) return { left, played, last: bag.last };
  const playedSet = new Set(played);
  for (const id of unique(ids, (id) => !leftSet.has(id) && !playedSet.has(id))) {
    left.splice(Math.floor(random() * (left.length + 1)), 0, id);
  }
  return { left, played, last: bag.last };
}

export function drawFromBag(bag: MusicBag, ids: string[], random: Random): { id: string | null; bag: MusicBag } {
  if (!ids.length) return { id: null, bag };
  let next = syncBag(bag, ids, random);
  if (!next.left.length) next = { left: freshLap(ids, next.last, random), played: [], last: next.last };
  const [id, ...left] = next.left;
  return { id, bag: { left, played: [...next.played, id], last: id } };
}

export function markPlayed(bag: MusicBag, ids: string[], id: string, random: Random): MusicBag {
  const next = syncBag(bag, ids, random);
  return {
    left: next.left.filter((x) => x !== id),
    played: next.played.includes(id) || !ids.includes(id) ? next.played : [...next.played, id],
    last: id,
  };
}

export function nextInOrder(ids: string[], last: string | null): string | null {
  if (!ids.length) return null;
  return ids[(ids.indexOf(last ?? "") + 1) % ids.length];
}

export function pickNext(bag: MusicBag, ids: string[], broken: ReadonlySet<string>, shuffle: boolean, random: Random): { id: string | null; bag: MusicBag } {
  const playable = ids.filter((id) => !broken.has(id));
  if (!playable.length) return { id: null, bag };
  if (shuffle) return drawFromBag(bag, playable, random);
  let id = bag.last;
  for (let i = 0; i < ids.length; i++) {
    id = nextInOrder(ids, id);
    if (id && !broken.has(id)) break;
  }
  return id ? { id, bag: markPlayed(bag, playable, id, random) } : { id: null, bag };
}

export function readBag(raw: unknown): MusicBag {
  const bag = raw as Partial<MusicBag> | null;
  const list = (v: unknown) => (Array.isArray(v) ? unique(v.filter((id): id is string => typeof id === "string" && VIDEO_ID.test(id)), () => true).slice(0, MAX_STORED) : []);
  const last = typeof bag?.last === "string" && VIDEO_ID.test(bag.last) ? bag.last : null;
  return { left: list(bag?.left), played: list(bag?.played), last };
}

const PLAYER_ERRORS: Record<number, string> = {
  2: "the video id was not accepted",
  5: "the player could not play it",
  100: "the video was removed or is private",
  101: "the owner does not allow playing it outside YouTube",
  150: "the owner does not allow playing it outside YouTube",
  153: "the player was refused without a referrer",
};

export function playerErrorReason(code: number): string {
  return PLAYER_ERRORS[code] ?? `player error ${code}`;
}

const BROKEN_CODES = new Set([2, 100, 101, 150]);

export function errorEffect(code: number): "broken" | "skip" {
  return BROKEN_CODES.has(code) ? "broken" : "skip";
}

export function errorBelongs(at: { requested: string | null; current: string | null; urlId: string | null; stateSinceLoad: boolean }): boolean {
  if (!at.requested || at.current !== at.requested) return false;
  if (at.urlId) return at.urlId === at.requested;
  return at.stateSinceLoad;
}

export function skipsExhausted(failedInARow: ReadonlySet<string>, playable: string[]): boolean {
  return playable.every((id) => failedInARow.has(id));
}
