export function streamUrl(lastSeq: number): string {
  return lastSeq > 0 ? `/api/world/stream?after=${lastSeq}` : "/api/world/stream";
}

export function newerSeq(current: number, seq: unknown): number {
  return typeof seq === "number" && Number.isFinite(seq) && seq > current ? seq : current;
}

export function resyncHead(data: string): number | null {
  try {
    const head = (JSON.parse(data) as { head?: unknown }).head;
    return typeof head === "number" && Number.isFinite(head) ? head : null;
  } catch {
    return null;
  }
}
