"use client";

import { useSyncExternalStore } from "react";
import dynamic from "next/dynamic";
import { WorldSplash } from "./world-splash";

const WorldGame = dynamic(() => import("./world-game"), {
  ssr: false,
  loading: () => <WorldSplash />,
});

export function WorldLoader() {
  const client = useSyncExternalStore(
    () => () => {},
    () => true,
    () => false,
  );
  if (!client) return <WorldSplash />;
  return <WorldGame />;
}
