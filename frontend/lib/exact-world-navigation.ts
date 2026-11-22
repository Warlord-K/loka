import type { RegionDetail } from "./loka";
import type { WorldItem } from "./world";

export type ExactWorldLink = { kind: "team" | "list" | "place" | "region"; id: string };

export type ExactMapDestination = {
  regionId: string;
  region: RegionDetail | null;
  place: WorldItem | null;
  label: string;
};

type ExactDestination = ExactWorldLink & { map?: ExactMapDestination };

export async function openExactDestination(
  link: ExactWorldLink,
  read: {
    team: (id: string) => Promise<{ id: string }>;
    item: (id: string) => Promise<WorldItem>;
    region: (id: string) => Promise<RegionDetail>;
    home: string;
  },
  isCurrent: () => boolean,
  actions: {
    closePanels: () => boolean | void;
    updateLink: (kind: ExactWorldLink["kind"], id: string) => void;
    showTeam: () => void;
    showList: () => void;
    showMap: (map: ExactMapDestination) => void;
    unavailable: () => void;
  },
): Promise<void> {
  let target: ExactDestination;
  try {
    if (link.kind === "team") {
      const team = await read.team(link.id);
      target = { kind: link.kind, id: team.id };
    } else if (link.kind === "region") {
      const region = await read.region(link.id);
      target = { kind: link.kind, id: region.region.id, map: { regionId: region.region.id, region, place: null, label: region.region.name } };
    } else {
      const item = await read.item(link.id);
      if (!isCurrent()) return;
      if (link.kind === "list") {
        if (item.kind !== "list" && item.kind !== "timer") throw new Error("wrong kind");
        target = { kind: link.kind, id: item.id };
      } else {
        if (item.kind !== "building" && item.kind !== "npc") throw new Error("wrong kind");
        const regionId = item.region_id ?? read.home;
        const region = regionId === read.home ? null : await read.region(regionId);
        target = { kind: link.kind, id: item.id, map: { regionId, region, place: item, label: item.name } };
      }
    }
  } catch {
    if (isCurrent()) actions.unavailable();
    return;
  }
  if (!isCurrent()) return;
  if (actions.closePanels() === false) return;
  actions.updateLink(target.kind, target.id);
  if (target.kind === "team") actions.showTeam();
  else if (target.kind === "list") actions.showList();
  else if (target.map) actions.showMap(target.map);
}

export function revealExactMap(
  destination: ExactMapDestination,
  currentRegion: string,
  engine: { currentPlace: () => string | null; leaveRoom: (instant: boolean) => void } | null,
  travel: (destination: ExactMapDestination) => void,
  reveal: (place: WorldItem | null) => void,
): void {
  if (destination.regionId !== currentRegion) {
    travel(destination);
    return;
  }
  if (!engine) return;
  if (engine.currentPlace()) engine.leaveRoom(true);
  reveal(destination.place);
}
