const FIRST_WAIT_MS = 5_000;
const LONGEST_WAIT_MS = 5 * 60_000;
const REFUSED = new Set([401, 403, 429]);

type Refusal = { status: number; until: number; wait: number };

const refusals = new Map<string, Refusal>();
let installed = false;

function keyOf(input: RequestInfo | URL, method: string): string | null {
  if (method !== "GET") return null;
  let url: URL;
  try {
    url = new URL(input instanceof Request ? input.url : String(input), window.location.href);
  } catch {
    return null;
  }
  if (url.origin !== window.location.origin || !url.pathname.startsWith("/api/")) return null;
  return url.pathname;
}

function retryAfterMs(res: Response): number {
  const header = res.headers.get("retry-after");
  const secs = header ? Number(header) : NaN;
  return Number.isFinite(secs) && secs > 0 ? secs * 1000 : 0;
}

export function clearRefusals() {
  refusals.clear();
}

export function installRefusalBackoff() {
  if (installed || typeof window === "undefined") return;
  installed = true;
  const original = window.fetch.bind(window);
  window.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
    const key = keyOf(input, method);
    if (!key) return original(input, init);
    const held = refusals.get(key);
    if (held && Date.now() < held.until) {
      return new Response(JSON.stringify({ error: "Waiting before asking again." }), {
        status: held.status,
        headers: { "content-type": "application/json", "x-loka-backoff": "1" },
      });
    }
    const res = await original(input, init);
    if (REFUSED.has(res.status)) {
      const wait = Math.min(Math.max((held?.wait ?? 0) * 2, FIRST_WAIT_MS, retryAfterMs(res)), LONGEST_WAIT_MS);
      refusals.set(key, { status: res.status, until: Date.now() + wait, wait });
    } else if (res.ok && held) {
      refusals.delete(key);
    }
    return res;
  };
}
