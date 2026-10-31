"use client";

import { useCallback, useEffect, useState } from "react";

export function useCopy(ms = 1400): [boolean, (text: string) => Promise<boolean>] {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const id = window.setTimeout(() => setCopied(false), ms);
    return () => window.clearTimeout(id);
  }, [copied, ms]);
  const copy = useCallback(async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      return true;
    } catch {
      setCopied(false);
      return false;
    }
  }, []);
  return [copied, copy];
}

export function fileSlug(name: string, fallback = "loka"): string {
  const slug = name
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 80);
  return slug || fallback;
}

export function downloadText(text: string, filename: string, type = "text/markdown;charset=utf-8") {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

export function checklistMarkdown(title: string, entries: { text: string; done: boolean }[]): string {
  const lines = entries.map((e) => `- [${e.done ? "x" : " "}] ${e.text}`);
  return [`## ${title}`, "", ...lines].join("\n");
}
