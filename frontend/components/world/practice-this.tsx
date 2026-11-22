"use client";

import { useCallback, useRef, useState } from "react";
import { LOKA_ROUTES, openAcademy, type Practiced } from "@/lib/loka";

export const PRACTICE_THIS_HINT = "Practice this in the Academy: a lesson with a coach, made once and opened again later";

export async function practiceThis(reportId: string): Promise<Practiced> {
  const res = await fetch(LOKA_ROUTES.practiceThis(reportId), { method: "POST", cache: "no-store" });
  if (!res.ok) {
    const text = (await res.text().catch(() => "")).trim();
    throw new Error(text && text.length < 300 && !text.startsWith("{") ? text : `Could not make the lesson (${res.status}).`);
  }
  return (await res.json()) as Practiced;
}

export function usePracticeThis(reportId: string) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const running = useRef(false);
  const run = useCallback(() => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    setError(null);
    practiceThis(reportId)
      .then((made) => openAcademy({ challengeId: made.challenge_id }))
      .catch((err: unknown) => setError(err instanceof Error ? err.message : "Could not make the lesson."))
      .finally(() => {
        running.current = false;
        setBusy(false);
      });
  }, [reportId]);
  return { busy, error, run };
}
