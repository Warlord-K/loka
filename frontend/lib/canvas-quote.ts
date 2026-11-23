export const MAX_QUOTED = 12;
export const CALL_ABOUT_EVENT = "world:call-about";

export interface QuotedItem {
  id: string;
  kind: string;
  label: string;
}

export function quoteItems(report: { id: string; title: string }, items: QuotedItem[]): { text: string; label: string; source: string } | null {
  const chosen = items.slice(0, MAX_QUOTED);
  if (!chosen.length) return null;
  const lines = chosen.map((i) => (i.label ? `${i.kind}: ${i.label}` : i.kind));
  return {
    text: (chosen.length === 1 ? lines[0] : lines.map((l) => `- ${l}`).join("\n")).slice(0, 2000),
    label: `${chosen.length === 1 ? chosen[0].kind : `${chosen.length} items`} on workspace "${report.title}"`.slice(0, 120),
    source: `report:${report.id}#${chosen.map((i) => i.id).join("+")}`,
  };
}

export function readAbout(raw: unknown): string | null {
  const about = raw && typeof raw === "object" ? (raw as { about?: unknown }).about : null;
  return typeof about === "string" && /^report:[0-9a-f-]{6,36}$/i.test(about) ? about : null;
}
