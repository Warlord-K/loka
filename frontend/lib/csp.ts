const IMAGE_HOSTS = ["https://avatars.githubusercontent.com", "https://i.ytimg.com"];
const FRAME_HOSTS = ["https://www.youtube.com", "https://www.youtube-nocookie.com", "https://*.notion.site"];
const CONNECT_HOSTS = [
  "https://huggingface.co",
  "https://*.huggingface.co",
  "https://*.hf.co",
  "https://cdn.jsdelivr.net",
  "wss://fal.run",
  "ipc:",
  "http://ipc.localhost",
];

function origins(raw: string | undefined): string[] {
  if (!raw) return [];
  try {
    const url = new URL(raw);
    const socket = url.protocol === "https:" ? "wss:" : "ws:";
    return [url.origin, `${socket}//${url.host}`];
  } catch {
    return [];
  }
}

export function apiOrigins(env: Record<string, string | undefined>): string[] {
  return [...new Set([env.API_URL, env.NEXT_PUBLIC_MCP_URL].flatMap(origins))];
}

export function contentSecurityPolicy(nonce: string | null, options: { dev: boolean; api: string[] }): string {
  const script = nonce
    ? ["'self'", `'nonce-${nonce}'`, "'strict-dynamic'", "'wasm-unsafe-eval'", ...(options.dev ? ["'unsafe-eval'"] : [])]
    : ["'self'", "'unsafe-inline'", "'wasm-unsafe-eval'", ...(options.dev ? ["'unsafe-eval'"] : [])];
  const connect = ["'self'", "blob:", ...CONNECT_HOSTS, ...options.api, ...(options.dev ? ["ws:"] : [])];
  return [
    "default-src 'self'",
    `script-src ${script.join(" ")}`,
    "style-src 'self' 'unsafe-inline'",
    `img-src 'self' data: blob: ${IMAGE_HOSTS.join(" ")}`,
    "font-src 'self' data:",
    "media-src 'self' data: blob:",
    `connect-src ${connect.join(" ")}`,
    "worker-src 'self' blob:",
    `frame-src ${FRAME_HOSTS.join(" ")}`,
    "manifest-src 'self'",
    "form-action 'self'",
    "frame-ancestors 'none'",
    "object-src 'none'",
    "base-uri 'self'",
  ].join("; ");
}

export function newNonce(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let raw = "";
  for (const b of bytes) raw += String.fromCharCode(b);
  return btoa(raw);
}
