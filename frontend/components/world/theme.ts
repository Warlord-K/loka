"use client";

import { useCallback, useEffect, useState } from "react";
import { THEME_KEY } from "@/lib/theme";

export type ThemeMode = "system" | "light" | "dark";

function readMode(): ThemeMode {
  try {
    const stored = localStorage.getItem(THEME_KEY);
    return stored === "light" || stored === "dark" ? stored : "system";
  } catch {
    return "system";
  }
}

export function useThemeMode(): [ThemeMode, (mode: ThemeMode) => void] {
  const [mode, setModeState] = useState<ThemeMode>("system");
  useEffect(() => setModeState(readMode()), []);
  const setMode = useCallback((next: ThemeMode) => {
    try {
      if (next === "system") localStorage.removeItem(THEME_KEY);
      else localStorage.setItem(THEME_KEY, next);
    } catch {}
    (window as unknown as { __worldTheme?: () => void }).__worldTheme?.();
    setModeState(next);
  }, []);
  return [mode, setMode];
}
