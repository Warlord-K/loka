const EXACT_ID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const FEATURE_ITEM = /^[A-Za-z0-9_-]{1,40}$/;

export type FeatureLink =
  | { kind: "workbench"; sessionId: string }
  | { kind: "workspace"; reportId: string; item?: string };

export function validFeatureItem(item: string | null | undefined): item is string {
  return typeof item === "string" && FEATURE_ITEM.test(item);
}

export function workspaceHref(origin: string, reportId: string, item?: string): string {
  const url = new URL("/world", origin);
  url.searchParams.set("report", reportId);
  if (item) url.searchParams.set("item", item);
  url.searchParams.set("workspace", "1");
  return url.toString();
}

export function featureLinkFromHref(href: string | null | undefined, origin: string): FeatureLink | null {
  if (!href || !/^https?:/i.test(href)) return null;
  try {
    const url = new URL(href);
    if (url.origin !== origin || url.pathname.replace(/\/$/, "") !== "/world") return null;
    const sessionId = url.searchParams.get("workbench");
    if (sessionId && EXACT_ID.test(sessionId) && url.searchParams.get("diff") === sessionId) {
      return { kind: "workbench", sessionId: sessionId.toLowerCase() };
    }
    const reportId = url.searchParams.get("report");
    if (reportId && EXACT_ID.test(reportId) && url.searchParams.get("workspace") === "1") {
      const item = url.searchParams.get("item");
      return validFeatureItem(item) ? { kind: "workspace", reportId: reportId.toLowerCase(), item } : { kind: "workspace", reportId: reportId.toLowerCase() };
    }
  } catch {}
  return null;
}

export function externalFeatureHref(href: string): boolean {
  if (!/^https?:/i.test(href)) return false;
  try {
    const url = new URL(href);
    return url.searchParams.has("workspace") || url.searchParams.has("workbench") || url.searchParams.has("diff");
  } catch {
    return false;
  }
}

export function consumeDiffHref(href: string, sessionId: string): string | null {
  try {
    const url = new URL(href);
    if (url.searchParams.get("diff")?.toLowerCase() !== sessionId.toLowerCase()) return null;
    url.searchParams.delete("diff");
    const query = url.searchParams.toString();
    return `${url.pathname}${query ? `?${query}` : ""}${url.hash}`;
  } catch {
    return null;
  }
}
