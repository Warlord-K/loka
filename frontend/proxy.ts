import { NextResponse, type NextRequest } from "next/server";
import { apiOrigins, contentSecurityPolicy, newNonce } from "./lib/csp";

const API_ORIGINS = apiOrigins(process.env);
const DEV = process.env.NODE_ENV !== "production";

export default function proxy(req: NextRequest): Response {
  if (req.nextUrl.pathname === "/") return NextResponse.redirect(new URL("/world", req.nextUrl));
  const nonce = newNonce();
  const policy = contentSecurityPolicy(nonce, { dev: DEV, api: API_ORIGINS });
  const headers = new Headers(req.headers);
  headers.set("x-nonce", nonce);
  headers.set("Content-Security-Policy", policy);
  const res = NextResponse.next({ request: { headers } });
  res.headers.set("Content-Security-Policy", policy);
  return res;
}

export const config = {
  matcher: ["/((?!api/|webhooks/|mcp$|_next/static|_next/image|favicon.ico|manifest.webmanifest|.*\\.png$|.*\\.ico$|.*\\.svg$|.*\\.jpg$|.*\\.jpeg$|.*\\.webp$).*)"],
};
