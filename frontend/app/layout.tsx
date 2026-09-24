import type { Metadata, Viewport } from "next";
import { headers } from "next/headers";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import { Providers } from "./providers";
import { THEME_SCRIPT } from "@/lib/theme";
import { TEXT_SCALE_SCRIPT } from "@/lib/text-scale";
import "./globals.css";

export const metadata: Metadata = {
  title: { default: "Loka", template: "%s · Loka" },
  description: "Your town of AI agents that take quests and ship real work.",
  applicationName: "Loka",
  formatDetection: { telephone: false, email: false, address: false },
  robots: { index: false, follow: false },
  icons: {
    icon: [
      { url: "/icon.svg", type: "image/svg+xml" },
      { url: "/favicon.ico", sizes: "any" },
      { url: "/favicon-32.png", sizes: "32x32", type: "image/png" },
      { url: "/favicon-16.png", sizes: "16x16", type: "image/png" },
      { url: "/icon-192.png", sizes: "192x192", type: "image/png" },
    ],
    apple: [{ url: "/apple-touch-icon.png", sizes: "180x180" }],
  },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  maximumScale: 1,
  userScalable: false,
  themeColor: "#141238",
};

export default async function RootLayout({ children }: { children: React.ReactNode }) {
  const nonce = (await headers()).get("x-nonce") ?? undefined;
  return (
    <html lang="en" className={`dark overscroll-none ${GeistSans.variable} ${GeistMono.variable}`} suppressHydrationWarning>
      <head>
        <script nonce={nonce} dangerouslySetInnerHTML={{ __html: THEME_SCRIPT + TEXT_SCALE_SCRIPT }} />
      </head>
      <body className={`${GeistSans.className} overscroll-none`}>
        <Providers>
          {children}
        </Providers>
      </body>
    </html>
  );
}
