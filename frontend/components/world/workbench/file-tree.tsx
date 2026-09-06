"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowsClockwise, CaretDown, CaretRight, FilePlus, FolderPlus, PencilSimple, Trash } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { PIXEL } from "../styles";
import { wb, type WbChange, type WbEntry } from "./bridge";

export const STATUS_COLOR: Record<string, string> = {
  M: "text-[#e8b64a]",
  A: "text-[#7be06b]",
  U: "text-[#7be06b]",
  D: "text-[#e07a5f]",
  R: "text-[#6bc7e8]",
  C: "text-[#6bc7e8]",
  T: "text-[#c39bff]",
};

export const STATUS_WORD: Record<string, string> = {
  M: "Modified",
  A: "Added",
  U: "Untracked",
  D: "Deleted",
  R: "Renamed",
  C: "Copied",
  T: "Type changed",
};

export function baseName(path: string): string {
  return path.split("/").pop() ?? path;
}

function parentOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

type Draft = { mode: "file" | "folder" | "rename"; dir: string; path?: string; value: string };

export function FileTree({
  sessionKey,
  changes,
  active,
  refresh,
  expanded,
  onExpandedChange,
  onOpen,
  onOpenChange,
  onRenamed,
  onDeleted,
  onError,
}: {
  sessionKey: string;
  changes: WbChange[];
  active: string | null;
  refresh: { nonce: number; paths: string[] };
  expanded: string[];
  onExpandedChange: (dirs: string[]) => void;
  onOpen: (path: string) => void;
  onOpenChange: (change: WbChange) => void;
  onRenamed: (from: string, to: string) => void;
  onDeleted: (path: string) => void;
  onError: (message: string) => void;
}) {
  const [dirs, setDirs] = useState<Record<string, WbEntry[]>>({});
  const [showChanges, setShowChanges] = useState(true);
  const [showFiles, setShowFiles] = useState(true);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [selectedDir, setSelectedDir] = useState("");
  const loaded = useRef(new Set<string>());
  const open = useMemo(() => new Set(expanded), [expanded]);

  const load = useCallback(
    async (dir: string) => {
      try {
        const list = await wb.list(sessionKey, dir);
        loaded.current.add(dir);
        setDirs((prev) => ({ ...prev, [dir]: list }));
      } catch (error) {
        loaded.current.delete(dir);
        setDirs((prev) => {
          const next = { ...prev };
          delete next[dir];
          return next;
        });
        if (dir === "") onError(error instanceof Error ? error.message : "Could not list files.");
      }
    },
    [sessionKey, onError],
  );

  useEffect(() => {
    loaded.current = new Set();
    setDirs({});
    void load("");
  }, [load]);

  useEffect(() => {
    for (const dir of expanded) if (!loaded.current.has(dir)) void load(dir);
  }, [expanded, load]);

  useEffect(() => {
    if (!refresh.nonce) return;
    const touched = new Set<string>();
    for (const path of refresh.paths) {
      touched.add(parentOf(path));
      touched.add(path);
    }
    for (const dir of touched) if (loaded.current.has(dir)) void load(dir);
    if (!refresh.paths.length) for (const dir of loaded.current) void load(dir);
  }, [refresh, load]);

  const status = useMemo(() => {
    const files = new Map<string, string>();
    const folders = new Set<string>();
    for (const change of changes) {
      files.set(change.path, change.status);
      let dir = parentOf(change.path);
      while (dir) {
        folders.add(dir);
        dir = parentOf(dir);
      }
    }
    return { files, folders };
  }, [changes]);

  const toggle = (dir: string) => {
    setSelectedDir(dir);
    onExpandedChange(open.has(dir) ? expanded.filter((d) => d !== dir) : [...expanded, dir]);
  };

  const submitDraft = async () => {
    if (!draft) return;
    const name = draft.value.trim();
    setDraft(null);
    if (!name || name.includes("/") && draft.mode === "rename") return;
    const target = draft.dir ? `${draft.dir}/${name}` : name;
    try {
      if (draft.mode === "rename" && draft.path) {
        if (target === draft.path) return;
        await wb.rename(sessionKey, draft.path, target);
        onRenamed(draft.path, target);
      } else {
        await wb.create(sessionKey, target, draft.mode === "folder");
        if (draft.dir && !open.has(draft.dir)) onExpandedChange([...expanded, draft.dir]);
        if (draft.mode === "file") onOpen(target);
      }
      await load(draft.dir);
    } catch (error) {
      onError(error instanceof Error ? error.message : "That did not work.");
    }
  };

  const remove = async (entry: WbEntry) => {
    if (!window.confirm(`Delete ${entry.path}${entry.is_dir ? " and everything in it" : ""}? This cannot be undone.`)) return;
    try {
      await wb.remove(sessionKey, entry.path);
      onDeleted(entry.path);
      await load(parentOf(entry.path));
    } catch (error) {
      onError(error instanceof Error ? error.message : "Could not delete it.");
    }
  };

  const draftRow = (dir: string, depth: number) =>
    draft && draft.mode !== "rename" && draft.dir === dir ? (
      <li style={{ paddingLeft: 8 + depth * 12 }} className="py-0.5 pr-2">
        <input
          autoFocus
          value={draft.value}
          onChange={(e) => setDraft({ ...draft, value: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === "Enter") void submitDraft();
            if (e.key === "Escape") {
              e.stopPropagation();
              setDraft(null);
            }
          }}
          onBlur={() => void submitDraft()}
          placeholder={draft.mode === "file" ? "new-file.ts" : "new-folder"}
          className="w-full rounded-md border border-[#19b8b8] bg-[#140d18] px-1.5 py-0.5 font-mono text-w-xs text-[#e9dcc6] outline-none"
        />
      </li>
    ) : null;

  const renderDir = (dir: string, depth: number): React.ReactNode => {
    const entries = dirs[dir];
    if (!entries) return depth === 0 ? <li className="px-3 py-2 text-w-xs text-[#9c8a9a]">Loading files...</li> : null;
    return (
      <>
        {draftRow(dir, depth)}
        {entries.map((entry) => {
          const isOpen = entry.is_dir && open.has(entry.path);
          const letter = status.files.get(entry.path);
          const changed = entry.is_dir && status.folders.has(entry.path);
          const renaming = draft?.mode === "rename" && draft.path === entry.path;
          return (
            <li key={entry.path}>
              {renaming ? (
                <div style={{ paddingLeft: 8 + depth * 12 }} className="py-0.5 pr-2">
                  <input
                    autoFocus
                    value={draft.value}
                    onChange={(e) => setDraft({ ...draft, value: e.target.value })}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") void submitDraft();
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        setDraft(null);
                      }
                    }}
                    onBlur={() => void submitDraft()}
                    className="w-full rounded-md border border-[#19b8b8] bg-[#140d18] px-1.5 py-0.5 font-mono text-w-xs text-[#e9dcc6] outline-none"
                  />
                </div>
              ) : (
                <div
                  role="treeitem"
                  aria-expanded={entry.is_dir ? isOpen : undefined}
                  aria-selected={active === entry.path}
                  tabIndex={0}
                  onClick={() => (entry.is_dir ? toggle(entry.path) : (setSelectedDir(parentOf(entry.path)), onOpen(entry.path)))}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") entry.is_dir ? toggle(entry.path) : onOpen(entry.path);
                  }}
                  style={{ paddingLeft: 6 + depth * 12 }}
                  className={cn(
                    "group flex cursor-pointer items-center gap-1 py-[3px] pr-2 text-w-xs outline-none hover:bg-[#2c2032] focus-visible:bg-[#2c2032]",
                    active === entry.path && "bg-[#33243a] text-white",
                  )}
                >
                  <span className="w-3 shrink-0 text-[#9c8a9a]">{entry.is_dir && (isOpen ? <CaretDown size={11} weight="bold" /> : <CaretRight size={11} weight="bold" />)}</span>
                  <span className={cn("min-w-0 flex-1 truncate", letter ? STATUS_COLOR[letter] : changed ? "text-[#e9c98a]" : "text-[#d8c9b2]", entry.is_symlink && "italic")}>{entry.name}</span>
                  <span className="hidden shrink-0 items-center gap-1 group-hover:flex [@media(hover:none)]:flex">
                    <button type="button" aria-label={`Rename ${entry.name}`} onClick={(e) => (e.stopPropagation(), setDraft({ mode: "rename", dir: parentOf(entry.path), path: entry.path, value: entry.name }))} className="text-[#9c8a9a] hover:text-white">
                      <PencilSimple size={12} />
                    </button>
                    <button type="button" aria-label={`Delete ${entry.name}`} onClick={(e) => (e.stopPropagation(), void remove(entry))} className="text-[#9c8a9a] hover:text-[#e07a5f]">
                      <Trash size={12} />
                    </button>
                  </span>
                  {letter ? (
                    <span className={cn("w-3 shrink-0 text-center font-mono text-w-2xs font-bold", STATUS_COLOR[letter])}>{letter}</span>
                  ) : changed ? (
                    <span className="mr-1 h-1.5 w-1.5 shrink-0 rounded-full bg-[#e8b64a]" />
                  ) : null}
                </div>
              )}
              {isOpen && <ul role="group">{renderDir(entry.path, depth + 1)}</ul>}
            </li>
          );
        })}
      </>
    );
  };

  const section = "flex w-full items-center gap-1 px-2 py-1.5 text-left text-w-2xs uppercase tracking-[0.12em] text-[#9c8a9a] hover:text-[#e9dcc6]";

  return (
    <div className="flex h-full min-h-0 flex-col bg-[#1d1422]">
      <div className="flex items-center justify-between border-b border-[#3a2a3d] px-2 py-1.5">
        <span className={cn(PIXEL, "text-w-xs text-[#e9dcc6]")}>Explorer</span>
        <span className="flex items-center gap-1.5 text-[#9c8a9a]">
          <button type="button" aria-label="New file" title="New file" onClick={() => setDraft({ mode: "file", dir: selectedDir, value: "" })} className="hover:text-white">
            <FilePlus size={14} />
          </button>
          <button type="button" aria-label="New folder" title="New folder" onClick={() => setDraft({ mode: "folder", dir: selectedDir, value: "" })} className="hover:text-white">
            <FolderPlus size={14} />
          </button>
          <button
            type="button"
            aria-label="Refresh"
            title="Refresh"
            onClick={() => {
              for (const dir of loaded.current) void load(dir);
            }}
            className="hover:text-white"
          >
            <ArrowsClockwise size={14} />
          </button>
        </span>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        <button type="button" className={section} onClick={() => setShowChanges((v) => !v)}>
          {showChanges ? <CaretDown size={10} weight="bold" /> : <CaretRight size={10} weight="bold" />}
          Changes
          <span className="ml-auto rounded-md bg-[#33243a] px-1.5 text-w-3xs text-[#e9dcc6]">{changes.length}</span>
        </button>
        {showChanges && (
          <ul className="mb-1">
            {changes.length === 0 && <li className="px-4 py-1 text-w-xs text-[#9c8a9a]">No changes against the base branch.</li>}
            {changes.map((change) => (
              <li key={change.path}>
                <button
                  type="button"
                  onClick={() => onOpenChange(change)}
                  title={`${STATUS_WORD[change.status] ?? change.status}${change.old_path ? ` from ${change.old_path}` : ""}`}
                  className={cn("flex w-full items-center gap-1.5 py-[3px] pl-4 pr-2 text-left text-w-xs hover:bg-[#2c2032]", active === change.path && "bg-[#33243a]")}
                >
                  <span className={cn("min-w-0 truncate", STATUS_COLOR[change.status], change.status === "D" && "line-through")}>{baseName(change.path)}</span>
                  <span className="min-w-0 flex-1 truncate text-w-2xs text-[#7d6b7b]">{parentOf(change.path)}</span>
                  <span className={cn("w-3 shrink-0 text-center font-mono text-w-2xs font-bold", STATUS_COLOR[change.status])}>{change.status}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
        <button
          type="button"
          className={section}
          onClick={() => {
            setShowFiles((v) => !v);
            setSelectedDir("");
          }}
        >
          {showFiles ? <CaretDown size={10} weight="bold" /> : <CaretRight size={10} weight="bold" />}
          Files
        </button>
        {showFiles && <ul role="tree">{renderDir("", 0)}</ul>}
      </div>
    </div>
  );
}
