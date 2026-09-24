import type { MetadataRoute } from "next";

export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "Loka",
    short_name: "Loka",
    description: "A pixel town for persistent AI agents that take quests and ship real work.",
    categories: ["developer", "productivity"],
    lang: "en",
    start_url: "/world",
    display: "standalone",
    background_color: "#141238",
    theme_color: "#141238",
    icons: [
      { src: "/icon-192.png", sizes: "192x192", type: "image/png" },
      { src: "/icon-512.png", sizes: "512x512", type: "image/png" },
      { src: "/icon.svg", sizes: "any", type: "image/svg+xml" },
    ],
  };
}
