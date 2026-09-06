import { desktopApi, isDesktopApp, type PlacedPath } from "@/lib/desktop";

export interface WbOpen {
  key: string;
  root: string;
  name: string;
  is_git: boolean;
  branch: string | null;
  base: string | null;
  head: string | null;
}

export interface WbEntry {
  name: string;
  path: string;
  is_dir: boolean;
  is_symlink: boolean;
}

export interface WbFile {
  text: string | null;
  sha: string;
  size: number;
  binary: boolean;
  too_large: boolean;
}

export interface WbWrite {
  saved: boolean;
  conflict: boolean;
  sha: string | null;
}

export type WbStatus = "M" | "A" | "D" | "R" | "C" | "U" | "T";

export interface WbChange {
  path: string;
  status: WbStatus;
  old_path: string | null;
}

export interface WbChanges {
  base: string | null;
  merge_base: string | null;
  files: WbChange[];
}

export interface WbBase {
  exists: boolean;
  text: string | null;
  binary: boolean;
}

export interface WbMatch {
  path: string;
  line: number;
  column: number;
  text: string;
}

export interface WbTerm {
  id: string;
  key: string;
  title: string;
  exited: boolean;
}

export type TermEvent = { type: "data"; data: string } | { type: "exit"; code: number | null };

interface TauriChannel<T> {
  onmessage: (message: T) => void;
}

function message(error: unknown): string {
  return typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.";
}

function invokeTauri<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const invoke = typeof window !== "undefined" ? window.__TAURI__?.core?.invoke : undefined;
  if (!invoke) return Promise.reject(new Error("desktop_unavailable"));
  return invoke<T>(command, args).catch((error: unknown) => {
    throw new Error(message(error));
  });
}

function channel<T>(onMessage: (message: T) => void): TauriChannel<T> {
  const Ctor = window.__TAURI__?.core?.Channel;
  if (!Ctor) throw new Error("desktop_unavailable");
  const made: TauriChannel<T> = new Ctor<T>();
  made.onmessage = onMessage;
  return made;
}

function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  return invokeTauri<T>(command, args);
}

export type WorkbenchSupport = "ready" | "update";

let support: Promise<WorkbenchSupport> | null = null;

export function workbenchSupport(): Promise<WorkbenchSupport> {
  if (!isDesktopApp()) return Promise.resolve("update");
  support ??= desktopApi
    .status()
    .then((status): WorkbenchSupport => (status.features?.includes("workbench") ? "ready" : "update"))
    .catch(() => "update" as WorkbenchSupport);
  return support;
}

export function sessionKeyFor(session: { working_dir?: string | null; task_id: string | null }): string | null {
  const dir = session.working_dir?.trim();
  if (dir) return dir;
  return session.task_id ? `tasks/${session.task_id}` : null;
}

export const wb = {
  open: (key: string, base: string | null) => call<WbOpen>("wb_open", { sessionKey: key, base }),
  list: (key: string, dir: string) => call<WbEntry[]>("wb_list", { sessionKey: key, dir }),
  files: (key: string) => call<string[]>("wb_files", { sessionKey: key }),
  read: (key: string, path: string) => call<WbFile>("wb_read", { sessionKey: key, path }),
  write: (key: string, path: string, text: string, baseSha: string | null) => call<WbWrite>("wb_write", { sessionKey: key, path, text, baseSha }),
  create: (key: string, path: string, dir: boolean) => call<void>("wb_create", { sessionKey: key, path, dir }),
  rename: (key: string, from: string, to: string) => call<void>("wb_rename", { sessionKey: key, from, to }),
  remove: (key: string, path: string) => call<void>("wb_delete", { sessionKey: key, path }),
  changes: (key: string, base: string | null) => call<WbChanges>("wb_changes", { sessionKey: key, base }),
  base: (key: string, path: string, base: string | null) => call<WbBase>("wb_base", { sessionKey: key, path, base }),
  find: (key: string, query: string, regex = false) => call<WbMatch[]>("wb_find", { sessionKey: key, query, mode: regex ? "regex" : "text" }),
  watch: (key: string, onBatch: (paths: string[]) => void): Promise<void> =>
    invokeTauri<void>("wb_watch", { sessionKey: key, onEvent: channel<{ paths: string[] }>((batch) => onBatch(batch.paths)) }),
  unwatch: (key: string): Promise<void> => invokeTauri<void>("wb_unwatch", { sessionKey: key }),
  termOpen: (key: string, cols: number, rows: number, id: string | null, onEvent: (event: TermEvent) => void): Promise<WbTerm> =>
    invokeTauri<WbTerm>("term_open", { sessionKey: key, cols, rows, id, onEvent: channel<TermEvent>(onEvent) }),
  homeTermOpen: (cols: number, rows: number, id: string | null, onEvent: (event: TermEvent) => void): Promise<WbTerm> =>
    invokeTauri<WbTerm>("home_term_open", { cols, rows, id, onEvent: channel<TermEvent>(onEvent) }),
  termList: (key: string) => call<WbTerm[]>("term_list", { sessionKey: key }),
  termWrite: (id: string, data: string) => call<void>("term_write", { id, data }),
  termResize: (id: string, cols: number, rows: number) => call<void>("term_resize", { id, cols, rows }),
  termClose: (id: string) => call<void>("term_close", { id }),
};

export type OpenWorkshopDetail = string | { sessionId?: string | null; taskId?: string | null; agentId?: string | null; file?: string | null; line?: number | null };

export interface OpenWorkshop {
  sessionId: string;
  agentId: string;
  runnerId: string | null;
  root: string;
}

let openWorkshop: OpenWorkshop | null = null;

export function setOpenWorkshop(next: OpenWorkshop | null): void {
  openWorkshop = next;
}

export function currentWorkshop(): OpenWorkshop | null {
  return openWorkshop;
}

export function requestWorkshop(detail: OpenWorkshopDetail): void {
  window.dispatchEvent(new CustomEvent<OpenWorkshopDetail>("world:open-workbench", { detail }));
}
