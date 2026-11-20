"use client";

import { quoteProps } from "./quote";
import { SOLO } from "@/lib/edition";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ArrowCounterClockwise, ArrowLeft, CaretDown, CaretRight, Check, ClockCounterClockwise, FileText, Folder, FolderOpen, Globe, Link as LinkIcon, ListBullets, Lock, Code, Copy, DownloadSimple, PencilSimple, Plus, Trash, Clock, Files, FolderSimple, Hash, Notebook, PushPinSimple, UserCircle, type Icon as PhosphorIcon, SidebarSimple } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { WindowHeader, EmptyState, GHOST_ICON_BTN } from "./settings-shell";
import { AgentFace } from "./channel-ui";
import { Keycap } from "./keycap";
import { worldUrl } from "./message-links";
import { worldApi, type WorldAgent } from "@/lib/world";
import {
  folderParts,
  headingsOf,
  REPORTS_STALE,
  reportsApi,
  wikify,
  ReportError,
  type ReportBrief,
  type ReportDetail,
  type Revision,
  type RevisionBrief,
} from "@/lib/reports";
import { RichText, useAgentHandles } from "./chat-kit";
import { RunCommandContext, type RunCommand } from "./run-command";
import { LocalPathContext } from "./local-path";
import { UiCard } from "./lazy";
import { FilterField } from "./filter-field";
import { RelativeTime } from "./relative-time";
import { ScopeToggle, useEveryone } from "./scope";
import { BTN_PRIMARY, BTN_SECONDARY, EASE, FOCUS, ICON_BTN, NAV_GLIDE, NAV_ITEM, NAV_OFF, NAV_ON, NAV_PILL, PANEL, PILL_INFO, PILL_NEUTRAL, SIDEBAR, ICON_BTN_MD, COUNT } from "./styles";
import Glide from "./glide";
import { Shimmer } from "./work-ui";
import { scrollWithin } from "@/lib/scroll";
import { escapeTaken } from "@/lib/escape";
import dynamic from "next/dynamic";
import { focusPointAt, type FocusPoint } from "@/lib/caret";
import { downloadText, fileSlug, useCopy } from "@/lib/copy";
import { PaneResizer, usePaneWidth } from "./pane-resizer";

const DocEditor = dynamic(() => import("./doc-editor").then((m) => m.DocEditor), {
  ssr: false,
  loading: () => (
    <p className="min-h-[40vh] text-w-sm text-w-muted">
      <Shimmer>Opening the editor</Shimmer>
    </p>
  ),
});

type Scope = { kind: "all" } | { kind: "pinned" } | { kind: "folder"; path: string } | { kind: "tag"; tag: string };
type Mode = "read" | "edit" | "history";
type Draft = { title: string; folder: string; tags: string; body: string };
type SaveState = "idle" | "saving" | "saved" | "failed";

function same(a: Draft, b: Draft): boolean {
  return a.title === b.title && a.body === b.body && a.folder === b.folder && a.tags === b.tags;
}

function draftOf(r: { title: string; folder: string; tags: string[]; body: string }): Draft {
  return { title: r.title, folder: r.folder, tags: r.tags.join(", "), body: r.body };
}

function grow(el: HTMLTextAreaElement | null) {
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}

interface FolderNode {
  name: string;
  path: string;
  count: number;
  children: FolderNode[];
}

const MAX_BYTES = 512 * 1024;
const reportCache: { list: ReportBrief[] | null } = { list: null };
const PROSE = "text-w-md leading-[1.75] text-w-text [&_li]:my-1 [&_p]:my-3 [&_blockquote]:my-4 [&_ul]:my-3 [&_ol]:my-3";
const TOOL_BTN = cn("inline-flex h-8 items-center gap-1.5 rounded-lg px-2.5 text-w-sm text-w-text2 transition-colors hover:bg-w-sand hover:text-w-strong", EASE, FOCUS, "focus-visible:ring-offset-0");
const TOOL_ICON = cn("grid h-8 w-8 place-items-center rounded-lg text-w-muted transition-colors hover:bg-w-sand hover:text-w-strong", EASE, FOCUS, "focus-visible:ring-offset-0");

function folderTree(reports: ReportBrief[]): FolderNode[] {
  const root: FolderNode = { name: "", path: "", count: 0, children: [] };
  for (const r of reports) {
    let node = root;
    const parts = folderParts(r.folder);
    parts.forEach((part, i) => {
      const path = parts.slice(0, i + 1).join("/");
      let child = node.children.find((c) => c.path === path);
      if (!child) {
        child = { name: part, path, count: 0, children: [] };
        node.children.push(child);
      }
      child.count += 1;
      node = child;
    });
  }
  const sort = (nodes: FolderNode[]) => {
    nodes.sort((a, b) => a.name.localeCompare(b.name));
    nodes.forEach((n) => sort(n.children));
  };
  sort(root.children);
  return root.children;
}

function inScope(r: ReportBrief, scope: Scope): boolean {
  if (scope.kind === "pinned") return r.pinned;
  if (scope.kind === "folder") return r.folder === scope.path || r.folder.startsWith(`${scope.path}/`);
  if (scope.kind === "tag") return r.tags.includes(scope.tag);
  return true;
}

function sizeLabel(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : `${Math.round(bytes / 1024)} KB`;
}

function readMinutes(bytes: number): string {
  return `${Math.max(1, Math.round(bytes / 1300))} min read`;
}

function excerptText(text: string): string {
  return text
    .replace(/```[\s\S]*?(```|$)/g, " ")
    .replace(/^#+\s.*$/gm, " ")
    .replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_, t: string, a?: string) => a ?? t)
    .replace(/[*`>|#]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function withoutTitle(body: string, title: string): string {
  const match = body.match(/^\s*#\s+(.+?)\s*\n/);
  if (!match || match[1].trim().toLowerCase() !== title.trim().toLowerCase()) return body;
  return body.slice(match[0].length);
}

function asMarkdown(title: string, body: string): string {
  const text = withoutTitle(body, title).replace(/^\s+/, "");
  return `# ${title.trim()}\n\n${text}`.replace(/\s+$/, "") + "\n";
}

function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

function FolderRow({ node, depth, scope, open, onToggle, onPick }: { node: FolderNode; depth: number; scope: Scope; open: Set<string>; onToggle: (path: string) => void; onPick: (path: string) => void }) {
  const expanded = open.has(node.path);
  const active = scope.kind === "folder" && scope.path === node.path;
  return (
    <li>
      <div className={cn("relative flex items-center gap-0.5 rounded-lg pr-2.5 text-w-sm transition-colors", EASE, active ? NAV_ON : NAV_OFF)} style={{ paddingLeft: depth === 0 && !node.children.length ? "0.625rem" : `${0.25 + depth * 0.85}rem` }}>
        <button
          type="button"
          onClick={() => onToggle(node.path)}
          aria-label={expanded ? `Collapse ${node.name}` : `Expand ${node.name}`}
          className={cn("grid h-8 w-5 shrink-0 place-items-center text-w-muted", !node.children.length && (depth === 0 ? "hidden" : "invisible"))}
        >
          {expanded ? <CaretDown size={11} weight="bold" /> : <CaretRight size={11} weight="bold" />}
        </button>
        <button type="button" onClick={() => onPick(node.path)} className="flex h-8 min-w-0 flex-1 items-center gap-2 text-left">
          {expanded || active ? <FolderOpen size={16} weight={active ? "fill" : "regular"} className={cn("shrink-0", active ? "text-w-link" : "text-w-muted")} /> : <Folder size={16} className="shrink-0 text-w-muted" />}
          <span className="min-w-0 flex-1 truncate">{node.name}</span>
          <span className="text-w-2xs font-normal tabular-nums text-w-muted">{node.count}</span>
        </button>
      </div>
      {expanded && node.children.length > 0 && (
        <ul className="mt-0.5 space-y-0.5">
          {node.children.map((c) => (
            <FolderRow key={c.path} node={c} depth={depth + 1} scope={scope} open={open} onToggle={onToggle} onPick={onPick} />
          ))}
        </ul>
      )}
    </li>
  );
}

function ScopeButton({ active, onClick, icon: ScopeIcon, label, count }: { active: boolean; onClick: () => void; icon: PhosphorIcon; label: string; count: number }) {
  return (
    <button type="button" onClick={onClick} aria-current={active ? "page" : undefined} className={cn(NAV_ITEM, active ? NAV_ON : NAV_OFF)}>
      <ScopeIcon size={16} weight={active ? "fill" : "regular"} className={cn("shrink-0", active ? "text-w-link" : "text-w-muted")} />
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="text-w-2xs font-normal tabular-nums text-w-muted">{count}</span>
    </button>
  );
}

function SideHeading({ children }: { children: ReactNode }) {
  return <h3 className="mb-1.5 mt-5 px-2.5 text-w-xs font-medium text-w-muted">{children}</h3>;
}

function Property({ icon: PropIcon, label, children }: { icon: PhosphorIcon; label: string; children: ReactNode }) {
  return (
    <>
      <dt className="flex items-center gap-2 py-1.5 text-w-sm text-w-muted">
        <PropIcon size={15} className="shrink-0" />
        {label}
      </dt>
      <dd className="flex min-w-0 flex-wrap items-center gap-1.5 py-1.5 text-w-sm text-w-text">{children}</dd>
    </>
  );
}

function LinkList({ title, items, onOpen }: { title: string; items: ReportBrief[]; onOpen: (id: string) => void }) {
  if (!items.length) return null;
  return (
    <div className="mt-6 first:mt-0">
      <h3 className="mb-1.5 text-w-xs font-medium text-w-muted">{title}</h3>
      <ul className="space-y-0.5">
        {items.map((r) => (
          <li key={r.id}>
            <button type="button" onClick={() => onOpen(r.id)} className="group flex w-full items-start gap-2 rounded-lg px-2 py-1.5 text-left transition-colors hover:bg-w-sand/70">
              <FileText size={15} className="mt-0.5 shrink-0 text-w-muted group-hover:text-w-link" />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-w-sm font-medium text-w-text">{r.title}</span>
                <span className="line-clamp-1 text-w-xs leading-snug text-w-muted">{excerptText(r.excerpt)}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ReportCard({
  source,
  stateKey,
  report,
  author,
  handles,
  onOpenAgent,
}: {
  source: string;
  stateKey: string;
  report: { id: string; title: string } | null;
  author: WorldAgent | undefined;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
}) {
  const spec = useMemo(() => {
    try {
      return JSON.parse(source) as unknown;
    } catch {
      return null;
    }
  }, [source]);
  const [sent, setSent] = useState(false);
  const [failed, setFailed] = useState(false);
  const answer = async (text: string) => {
    if (!author || !report) return false;
    setFailed(false);
    try {
      await worldApi.send(author.id, `About report:${report.id} "${report.title}":\n\n${text}`);
      setSent(true);
      return true;
    } catch {
      setFailed(true);
      return false;
    }
  };
  return (
    <div className="my-5 text-w-base leading-normal [&_li]:!my-0 [&_ol]:!my-0 [&_p]:!my-0 [&_ul]:!my-0">
      <UiCard
        spec={spec}
        stateKey={stateKey}
        handles={handles}
        onOpenAgent={onOpenAgent}
        canAnswer={!!author && !!report}
        answered={sent}
        answeredLabel={author ? `Sent to @${author.slug}` : "Sent"}
        onAnswer={answer}
      />
      {failed && <p className="mt-1 text-w-xs text-w-danger">That did not reach @{author?.slug}. Try again.</p>}
    </div>
  );
}

export function ReportsPanel({
  agents,
  meId,
  initialId,
  sideToggle,
  onClose,
  onOpenAgent,
}: {
  agents: WorldAgent[];
  meId: string | null;
  initialId?: string | null;
  sideToggle?: ReactNode;
  onClose: () => void;
  onOpenAgent: (id: string) => void;
}) {
  const handles = useAgentHandles(agents);
  const everyone = useEveryone();
  const [all, setAll] = useState<ReportBrief[] | null>(() => reportCache.list);
  useEffect(() => {
    if (all) reportCache.list = all;
  }, [all]);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<ReportBrief[] | null>(null);
  const [scope, setScope] = useState<Scope>({ kind: "all" });
  const [openFolders, setOpenFolders] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(initialId ?? null);
  const [detail, setDetail] = useState<ReportDetail | null>(null);
  const [missing, setMissing] = useState(false);
  const [mode, setMode] = useState<Mode>("read");
  const [draft, setDraft] = useState<Draft | null>(null);
  const [source, setSource] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [revisions, setRevisions] = useState<RevisionBrief[] | null>(null);
  const [revision, setRevision] = useState<Revision | null>(null);
  const [copied, copy] = useCopy();
  const [copiedText, copyText] = useCopy();
  const [tocOpen, setTocOpen] = useState(false);
  const readerRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const libraryPane = usePaneWidth("reports.library", 18, 13, 34);
  const [libraryHidden, setLibraryHidden] = useState(false);
  useEffect(() => {
    try {
      setLibraryHidden(window.localStorage.getItem("world.reports.library") === "hidden");
    } catch {}
  }, []);
  const toggleLibrary = () =>
    setLibraryHidden((v) => {
      try {
        window.localStorage.setItem("world.reports.library", v ? "shown" : "hidden");
      } catch {}
      return !v;
    });
  const [focusPoint, setFocusPoint] = useState<FocusPoint | null>(null);
  const editing = mode === "edit";
  const dirty = useMemo(() => {
    if (!draft) return false;
    if (!detail || selected === null) return Boolean(draft.title.trim() || draft.body.trim());
    const r = detail.report;
    return draft.title !== r.title || draft.body !== r.body || draft.folder !== r.folder || draft.tags !== r.tags.join(", ");
  }, [draft, detail, selected]);

  const loadAll = useCallback(() => {
    reportsApi
      .list()
      .then(setAll)
      .catch(() => setAll((cur) => cur ?? []));
  }, []);

  const loadDetail = useCallback((id: string) => {
    reportsApi
      .get(id)
      .then((d) => {
        setDetail(d);
        setMissing(false);
      })
      .catch((e) => {
        if (e instanceof ReportError && e.status === 404) {
          setDetail(null);
          setMissing(true);
        }
      });
  }, []);

  useEffect(() => loadAll(), [loadAll]);

  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    let live = true;
    const t = window.setTimeout(() => {
      reportsApi
        .list({ query: q })
        .then((r) => live && setHits(r))
        .catch(() => live && setHits([]));
    }, 220);
    return () => {
      live = false;
      window.clearTimeout(t);
    };
  }, [query, all]);

  useEffect(() => {
    setRevision(null);
    setRevisions(null);
    setTocOpen(false);
    if (!selected) {
      setDetail(null);
      setMissing(false);
      return;
    }
    if (detail?.report.id !== selected) setDetail(null);
    loadDetail(selected);
    readerRef.current?.scrollTo({ top: 0 });
  }, [selected]);

  useEffect(() => {
    if (initialId) {
      setSelected(initialId);
      setMode("read");
      setDraft(null);
    }
  }, [initialId]);

  const detailRef = useRef(detail);
  detailRef.current = detail;
  const editingRef = useRef(editing);
  editingRef.current = editing;
  const selectedRef = useRef(selected);
  selectedRef.current = selected;
  useEffect(() => {
    const onStale = (e: Event) => {
      loadAll();
      const id = (e as CustomEvent<{ report_id?: string } | undefined>).detail?.report_id;
      const current = selectedRef.current;
      if (current && (!id || id === current) && !editingRef.current) loadDetail(current);
    };
    window.addEventListener(REPORTS_STALE, onStale);
    return () => window.removeEventListener(REPORTS_STALE, onStale);
  }, [loadAll, loadDetail]);

  useEffect(() => {
    if (mode !== "history" || !selected) return;
    reportsApi
      .revisions(selected)
      .then(setRevisions)
      .catch(() => setRevisions([]));
  }, [mode, selected, detail?.report.revision]);

  const draftRef = useRef(draft);
  draftRef.current = draft;
  const savingRef = useRef<Promise<boolean> | null>(null);

  const persist = useCallback(async (): Promise<boolean> => {
    const id = selectedRef.current;
    const current = draftRef.current;
    if (!id || !current) return true;
    if (savingRef.current) await savingRef.current;
    const snapshot = selectedRef.current === id ? draftRef.current ?? current : current;
    if (!snapshot.title.trim()) {
      setError("Give it a title first.");
      return false;
    }
    if (byteLength(snapshot.body) > MAX_BYTES) {
      setError("Reports are at most 512 KB. Split it into linked reports with [[Title]] links.");
      return false;
    }
    const tagList = snapshot.tags.split(",").map((t) => t.trim()).filter(Boolean);
    setSaveState("saving");
    const run = reportsApi
      .update(id, { title: snapshot.title, body: snapshot.body, folder: snapshot.folder, tags: tagList })
      .then((saved) => {
        setDetail((d) => (d && d.report.id === saved.id ? { ...d, report: { ...d.report, ...saved } } : d));
        setError(null);
        setSaveState("saved");
        loadAll();
        window.dispatchEvent(new CustomEvent(REPORTS_STALE, { detail: { report_id: saved.id, from_editor: true } }));
        return true;
      })
      .catch((e) => {
        setSaveState("failed");
        setError(e instanceof Error && e.message ? e.message : "Could not save. Your text is still here, try again.");
        return false;
      })
      .finally(() => {
        savingRef.current = null;
      });
    savingRef.current = run;
    return run;
  }, [loadAll]);

  useEffect(() => {
    if (mode !== "edit" || !selected || !draft || !dirty) return;
    const t = window.setTimeout(() => void persist(), 1200);
    return () => window.clearTimeout(t);
  }, [mode, selected, draft, dirty, persist]);

  const leaveEdit = useCallback(async (): Promise<boolean> => {
    if (selectedRef.current) {
      if (dirty && !(await persist())) return false;
    } else if (dirty && !window.confirm("Discard this new report?")) return false;
    setDraft(null);
    setMode("read");
    setError(null);
    setSource(false);
    setSaveState("idle");
    return true;
  }, [dirty, persist]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || escapeTaken(e)) return;
      e.preventDefault();
      if (mode === "edit") {
        void leaveEdit();
        return;
      }
      if (mode === "history") {
        setMode("read");
        setRevision(null);
        return;
      }
      onClose();
    };
    const onEditKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() !== "e" || e.metaKey || e.ctrlKey || e.altKey || mode !== "read" || !detailRef.current?.can_edit) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
      e.preventDefault();
      startEditRef.current();
    };
    window.addEventListener("keydown", onEditKey);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onEditKey);
    };
  }, [mode, dirty, leaveEdit, onClose]);

  const visible = useMemo(() => (all ?? []).filter((r) => everyone || r.owner_user_id === meId), [all, everyone, meId]);
  const othersCount = (all ?? []).length - visible.length;
  const tree = useMemo(() => folderTree(visible), [visible]);
  const tags = useMemo(() => {
    const counts = new Map<string, number>();
    for (const r of visible) for (const t of r.tags) counts.set(t, (counts.get(t) ?? 0) + 1);
    return [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
  }, [visible]);
  const folders = useMemo(() => [...new Set(visible.map((r) => r.folder).filter(Boolean))].sort(), [visible]);
  const shown = useMemo(() => {
    const ids = new Set(visible.map((r) => r.id));
    return (hits ?? visible).filter((r) => ids.has(r.id) && inScope(r, scope));
  }, [hits, visible, scope]);
  const headings = useMemo(() => (detail ? headingsOf(withoutTitle(revision?.body ?? detail.report.body, revision?.title ?? detail.report.title)) : []), [detail, revision]);
  const known = useMemo(() => {
    const titles = new Set((all ?? []).flatMap((r) => [r.title.toLowerCase(), r.slug]));
    return (title: string) => titles.has(title.toLowerCase());
  }, [all]);

  const author = (agentId: string | null, userId: string | null) => {
    const agent = agentId ? agents.find((a) => a.id === agentId) : undefined;
    if (agent)
      return (
        <button type="button" onClick={() => onOpenAgent(agent.id)} className="inline-flex items-center gap-1.5 rounded-md font-medium text-w-text transition-colors hover:text-w-link">
          <AgentFace agent={agent} size={18} />
          {agent.name}
        </button>
      );
    if (agentId) return <span>an agent</span>;
    return <span>{userId && userId === meId ? "you" : "someone"}</span>;
  };

  const select = async (id: string | null) => {
    if (editing && !(await leaveEdit())) return;
    setError(null);
    setMode("read");
    setSelected(id);
  };

  const openWiki = (title: string) => {
    const key = title.toLowerCase();
    const local = (all ?? []).find((r) => r.title.toLowerCase() === key || r.slug === key);
    if (local) {
      select(local.id);
      return;
    }
    reportsApi
      .find(title)
      .then((r) => select(r.id))
      .catch(() => {
        if (detail?.can_edit !== false && window.confirm(`There is no report called "${title}" yet. Start it now?`)) startNew(title, detail?.report.folder ?? "");
      });
  };

  const startNew = async (title = "", folder = scope.kind === "folder" ? scope.path : "") => {
    if (editing && !(await leaveEdit())) return;
    setSelected(null);
    setDetail(null);
    setDraft({ title, folder, tags: scope.kind === "tag" ? scope.tag : "", body: "" });
    setMode("edit");
    setSource(false);
    setSaveState("idle");
    setError(null);
  };

  const startEdit = (point: FocusPoint | null = null) => {
    if (!detail || revision) return;
    setFocusPoint(point);
    setDraft(draftOf(detail.report));
    setMode("edit");
    setSource(false);
    setSaveState("idle");
    setError(null);
  };

  const startEditRef = useRef(startEdit);
  startEditRef.current = startEdit;

  const save = async () => {
    if (selected) {
      await persist();
      return;
    }
    if (!draft || saving) return;
    if (!draft.title.trim()) {
      setError("Give it a title first.");
      return;
    }
    if (byteLength(draft.body) > MAX_BYTES) {
      setError("Reports are at most 512 KB. Split it into linked reports with [[Title]] links.");
      return;
    }
    setSaving(true);
    setError(null);
    const tagList = draft.tags.split(",").map((t) => t.trim()).filter(Boolean);
    try {
      const saved = await reportsApi.create({ title: draft.title, body: draft.body, folder: draft.folder, tags: tagList });
      selectedRef.current = saved.id;
      setSelected(saved.id);
      setDetail({ report: saved, backlinks: [], outlinks: [], can_edit: true });
      setDraft(draftOf(saved));
      setSaveState("saved");
      loadAll();
      window.dispatchEvent(new CustomEvent(REPORTS_STALE, { detail: { report_id: saved.id } }));
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : "Could not save. Try again.");
    } finally {
      setSaving(false);
    }
  };

  const patch = async (change: { pinned?: boolean; visibility?: "private" | "public" }) => {
    if (!detail) return;
    setError(null);
    await reportsApi.update(detail.report.id, change).catch(() => setError("That change did not save. Try again."));
    loadDetail(detail.report.id);
    loadAll();
  };

  const remove = async () => {
    if (!detail || !window.confirm(`Delete "${detail.report.title}" and its history for good?`)) return;
    setError(null);
    try {
      await reportsApi.remove(detail.report.id);
    } catch {
      setError("Could not delete this report. Try again.");
      return;
    }
    setSelected(null);
    loadAll();
    window.dispatchEvent(new CustomEvent(REPORTS_STALE, { detail: { report_id: detail.report.id } }));
  };

  const restore = async (n: number) => {
    if (!detail || !window.confirm(`Restore revision ${n}? The current text stays in the history.`)) return;
    setError(null);
    try {
      await reportsApi.restore(detail.report.id, n);
    } catch {
      setError(`Could not restore revision ${n}. Try again.`);
      return;
    }
    setRevision(null);
    setMode("read");
    loadDetail(detail.report.id);
    loadAll();
  };

  const copyLink = async () => {
    if (detail) await copy(worldUrl("report", detail.report.id));
  };

  const shownMarkdown = () => (detail ? asMarkdown(revision?.title ?? detail.report.title, revision?.body ?? detail.report.body) : "");

  const copyMarkdown = async () => {
    if (detail) await copyText(shownMarkdown());
  };

  const downloadMarkdown = () => {
    if (!detail) return;
    const title = revision?.title ?? detail.report.title;
    downloadText(shownMarkdown(), `${fileSlug(title, "report")}${revision ? `-revision-${revision.revision}` : ""}.md`);
  };

  const [activeHeading, setActiveHeading] = useState<string | null>(null);
  useEffect(() => {
    const box = readerRef.current;
    if (!box || headings.length < 2) {
      setActiveHeading(null);
      return;
    }
    let raf = 0;
    const measure = () => {
      raf = 0;
      const top = box.getBoundingClientRect().top + box.clientHeight * 0.25;
      let current: string | null = headings[0]?.id ?? null;
      for (const h of headings) {
        const el = box.querySelector<HTMLElement>(`#${CSS.escape(h.id)}`);
        if (!el) continue;
        if (el.getBoundingClientRect().top <= top) current = h.id;
        else break;
      }
      if (box.scrollTop + box.clientHeight >= box.scrollHeight - 4) current = headings[headings.length - 1].id;
      setActiveHeading(current);
    };
    const onScroll = () => {
      if (!raf) raf = requestAnimationFrame(measure);
    };
    measure();
    box.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      box.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(raf);
    };
  }, [headings]);
  const jump = (id: string) => {
    const el = readerRef.current?.querySelector(`#${CSS.escape(id)}`);
    scrollWithin(el, { block: "start", smooth: true });
    setActiveHeading(id);
    setTocOpen(false);
  };

  const toggleFolder = (path: string) =>
    setOpenFolders((cur) => {
      const next = new Set(cur);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  const pickFolder = (path: string) => {
    setScope({ kind: "folder", path });
    setOpenFolders((cur) => new Set([...cur, ...folderParts(path).map((_, i, parts) => parts.slice(0, i + 1).join("/"))]));
  };

  const readerOpen = Boolean(selected) || mode === "edit";

  const sidebar = (
    <nav aria-label="Report folders">
      <div className="space-y-0.5">
        <ScopeButton active={scope.kind === "all"} onClick={() => setScope({ kind: "all" })} icon={Files} label="All reports" count={visible.length} />
        {visible.some((r) => r.pinned) && (
          <ScopeButton active={scope.kind === "pinned"} onClick={() => setScope({ kind: "pinned" })} icon={PushPinSimple} label="Pinned" count={visible.filter((r) => r.pinned).length} />
        )}
      </div>
      {tree.length > 0 && (
        <>
          <SideHeading>Folders</SideHeading>
          <ul className="space-y-0.5">
            {tree.map((n) => (
              <FolderRow key={n.path} node={n} depth={0} scope={scope} open={openFolders} onToggle={toggleFolder} onPick={pickFolder} />
            ))}
          </ul>
        </>
      )}
      {tags.length > 0 && (
        <>
          <SideHeading>Tags</SideHeading>
          <div className="flex flex-wrap gap-1 px-2">
            {tags.slice(0, 30).map(([t, n]) => {
              const active = scope.kind === "tag" && scope.tag === t;
              return (
                <button
                  key={t}
                  type="button"
                  onClick={() => setScope(active ? { kind: "all" } : { kind: "tag", tag: t })}
                  aria-pressed={active}
                  className={cn(
                    "inline-flex items-center gap-0.5 rounded-[0.3rem] border-[1.5px] px-1.5 py-0.5 text-w-xs transition-colors duration-150",
                    FOCUS,
                    active ? "border-w-teal/60 bg-w-aqua font-medium text-w-link" : "border-w-line bg-w-paper/60 text-w-text2 hover:border-w-line2 hover:text-w-text",
                  )}
                >
                  <Hash size={10} weight="bold" className="opacity-50" />
                  {t}
                  <span className="ml-0.5 tabular-nums opacity-50">{n}</span>
                </button>
              );
            })}
          </div>
        </>
      )}
    </nav>
  );

  const scopeLabel = scope.kind === "all" ? "All reports" : scope.kind === "pinned" ? "Pinned" : scope.kind === "folder" ? folderParts(scope.path).slice(-1)[0] : `#${scope.tag}`;

  const list = (
    <div className={cn(query.trim() ? "" : "mt-5 border-t-2 border-dashed border-w-line pt-4")}>
      <h3 className="mb-2 flex items-center justify-between px-2.5 text-w-sm font-semibold text-w-strong">
        <span className="truncate">{query.trim() ? "Results" : scopeLabel}</span>
        {all !== null && <span className={COUNT}>{shown.length}</span>}
      </h3>
      <ul
        ref={listRef}
        className="space-y-0.5"
        onKeyDown={(e) => {
          if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
          const rows = [...(listRef.current?.querySelectorAll<HTMLButtonElement>("button[data-report]") ?? [])];
          const at = rows.indexOf(document.activeElement as HTMLButtonElement);
          if (at < 0) return;
          e.preventDefault();
          const next = rows[at + (e.key === "ArrowDown" ? 1 : -1)];
          if (next) {
            next.focus();
            void select(next.dataset.report ?? null);
          }
        }}
      >
        {all === null && (
          <li className="px-2.5 py-2 text-w-sm text-w-muted">
            <Shimmer>Opening the library</Shimmer>
          </li>
        )}
        {all !== null && shown.length === 0 && (
          <li className="px-2.5 py-2 text-w-sm text-w-muted">{query.trim() ? "No reports match." : visible.length ? "Nothing here yet." : "No reports yet."}</li>
        )}
        {shown.map((r) => {
          const agent = r.author_agent_id ? agents.find((a) => a.id === r.author_agent_id) : undefined;
          const on = selected === r.id;
          return (
            <li key={r.id}>
              <button
                type="button"
                data-report={r.id}
                onClick={() => void select(r.id)}
                aria-current={on}
                className={cn("w-full rounded-lg px-2.5 py-2.5 text-left transition-colors", FOCUS, "focus-visible:ring-offset-0", on ? "bg-w-paper ring-2 ring-inset ring-w-line2" : "hover:bg-w-paper/60")}
              >
                <span className="flex items-start gap-2">
                  <span className="min-w-0 flex-1 text-w-sm font-semibold leading-snug text-w-strong">{r.title}</span>
                  {r.pinned && <PushPinSimple size={12} weight="fill" className="mt-1 shrink-0 text-w-faint" aria-label="Pinned" />}
                </span>
                <span className="mt-0.5 line-clamp-2 text-w-xs leading-relaxed text-w-text2">{excerptText(r.excerpt) || "Empty"}</span>
                <span className="mt-1.5 flex min-w-0 items-center gap-1.5 text-w-2xs text-w-muted">
                  {agent && <AgentFace agent={agent} size={16} />}
                  <RelativeTime at={r.updated_at} className="shrink-0" />
                  {r.folder && (
                    <>
                      <span aria-hidden>·</span>
                      <span className="min-w-0 truncate">{r.folder}</span>
                    </>
                  )}
                  {!SOLO && r.visibility === "public" && <Globe size={11} weight="bold" className="ml-auto shrink-0" aria-label="Public" />}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );

  const report = detail?.report;
  const body = revision?.body ?? report?.body ?? "";
  const reportAuthor = report?.author_agent_id ? agents.find((a) => a.id === report.author_agent_id) : undefined;
  const liveCard = (source: string, index: number) => (
    <ReportCard source={source} stateKey={`report:${report?.id ?? "none"}:${revision ? `r${revision.revision}` : "live"}:${index}`} report={report && !revision ? report : null} author={reportAuthor} handles={handles} onOpenAgent={onOpenAgent} />
  );
  const reportRunner: RunCommand | null =
    reportAuthor && meId && reportAuthor.owner_user_id === meId && !revision
      ? {
          where: reportAuthor.runner_name ? `on ${reportAuthor.runner_name}, output goes to ${reportAuthor.name}'s chat` : `on ${reportAuthor.name}'s device, output goes to its chat`,
          who: reportAuthor.name,
          run: async (command: string, runOn?: string) => {
            try {
              await worldApi.runCommand(reportAuthor.id, command, runOn);
            } catch (err) {
              const status = err instanceof Error ? parseInt(err.message, 10) : NaN;
              if (status === 409) throw new Error(`${reportAuthor.name}'s device is offline or not set, so the command could not run. Check Devices, then try again.`);
              if (status === 403) throw new Error("Only the owner of this agent can run commands.");
              throw new Error("That did not start. Try again.");
            }
          },
        }
      : null;

  const titles = useMemo(() => (all ?? []).map((r) => r.title), [all]);
  const focusBody = () => {
    const box = readerRef.current;
    const target = box?.querySelector<HTMLElement>(".ProseMirror") ?? box?.querySelector<HTMLTextAreaElement>("textarea[data-source]");
    target?.focus();
  };
  const status =
    saveState === "saving" ? (
      <Shimmer>Saving</Shimmer>
    ) : saveState === "failed" ? (
      <span className="text-w-danger">Not saved</span>
    ) : selected && dirty ? (
      <span>Editing</span>
    ) : saveState === "saved" ? (
      <span className="inline-flex items-center gap-1">
        <Check size={12} weight="bold" className="text-w-ok" /> Saved
      </span>
    ) : selected ? (
      <span>Changes save as you type</span>
    ) : (
      <span>New report</span>
    );

  const editor = draft && (
    <div className="mx-auto flex w-full max-w-[72rem] gap-12 px-5 pb-20 pt-5 @xl/reader:px-10 @xl/reader:pt-6 @4xl/reader:px-14">
      <article className="mx-auto min-w-0 max-w-[72ch] flex-1">
        <header className="mb-6">
          <div className="mb-6 flex flex-wrap items-center gap-2">
            <span className="flex min-w-0 flex-1 items-center gap-1.5 text-w-xs text-w-muted" aria-live="polite">
              <PencilSimple size={13} className="shrink-0" />
              {status}
            </span>
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() => setSource((v) => !v)}
                aria-pressed={source}
                title={source ? "Back to the page view" : "Edit the raw markdown"}
                className={cn(TOOL_BTN, source && "bg-w-sand text-w-strong")}
              >
                <Code size={15} />
                <span className="hidden @md/reader:inline">Markdown</span>
              </button>
              {selected ? (
                <button type="button" onClick={() => void leaveEdit()} className={cn(BTN_PRIMARY, "ml-1")} title="Save and stop editing (Esc)">
                  Done
                </button>
              ) : (
                <>
                  <button type="button" onClick={() => void leaveEdit()} className={BTN_SECONDARY}>
                    Cancel
                  </button>
                  <button type="button" onClick={() => void save()} disabled={saving} className={BTN_PRIMARY}>
                    {saving ? <Shimmer>Creating</Shimmer> : "Create report"}
                  </button>
                </>
              )}
            </div>
          </div>
          <textarea
            ref={grow}
            rows={1}
            value={draft.title}
            onChange={(e) => {
              grow(e.target);
              setDraft({ ...draft, title: e.target.value.replace(/\n/g, " ") });
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || (e.key === "ArrowDown" && e.currentTarget.selectionStart === e.currentTarget.value.length)) {
                e.preventDefault();
                focusBody();
              }
              if ((e.metaKey || e.ctrlKey) && e.key === "s") {
                e.preventDefault();
                void save();
              }
            }}
            placeholder="Untitled"
            aria-label="Title"
            autoFocus={!draft.title}
            className="block w-full resize-none overflow-hidden bg-transparent text-[2.1rem] font-bold leading-[1.15] tracking-[-0.02em] text-w-strong outline-none placeholder:text-w-faint @xl/reader:text-[2.5rem]"
          />
          <dl className="mt-5 grid grid-cols-[7.5rem_minmax(0,1fr)] items-center gap-x-3 border-b-2 border-w-line pb-5 @xl/reader:grid-cols-[9rem_minmax(0,1fr)]">
            <Property icon={Folder} label="Folder">
              <input
                list="report-folders"
                value={draft.folder}
                onChange={(e) => setDraft({ ...draft, folder: e.target.value })}
                placeholder="Empty, like research/infra"
                aria-label="Folder"
                className="-mx-1.5 min-w-0 flex-1 rounded-md bg-transparent px-1.5 py-0.5 text-w-sm text-w-text outline-none transition-colors placeholder:text-w-faint hover:bg-w-sand/70 focus:bg-w-field focus:ring-2 focus:ring-w-teal/40"
              />
              <datalist id="report-folders">
                {folders.map((f) => (
                  <option key={f} value={f} />
                ))}
              </datalist>
            </Property>
            <Property icon={Hash} label="Tags">
              <input
                value={draft.tags}
                onChange={(e) => setDraft({ ...draft, tags: e.target.value })}
                placeholder="Empty, comma separated"
                aria-label="Tags"
                className="-mx-1.5 min-w-0 flex-1 rounded-md bg-transparent px-1.5 py-0.5 text-w-sm text-w-text outline-none transition-colors placeholder:text-w-faint hover:bg-w-sand/70 focus:bg-w-field focus:ring-2 focus:ring-w-teal/40"
              />
            </Property>
          </dl>
        </header>
        {source ? (
          <textarea
            ref={grow}
            data-source
            value={draft.body}
            onChange={(e) => {
              grow(e.target);
              setDraft({ ...draft, body: e.target.value });
            }}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "s") {
                e.preventDefault();
                void save();
              }
            }}
            placeholder="Write in markdown. Link other reports with [[Their title]]."
            aria-label="Report markdown"
            spellCheck
            className="block min-h-[40vh] w-full resize-none overflow-hidden bg-transparent font-mono text-w-base leading-relaxed text-w-text outline-none placeholder:text-w-faint"
          />
        ) : (
          <DocEditor
            key={selected ?? "new"}
            value={draft.body}
            onChange={(body) => setDraft((d) => (d ? { ...d, body } : d))}
            onLossy={() => {
              setSource(true);
              setError("This report uses formatting the page view cannot keep, so it opened as markdown.");
            }}
            onSave={() => void save()}
            titles={titles}
            autoFocus={Boolean(draft.title)}
            focusPoint={focusPoint}
          />
        )}
        {error && <p className="mt-4 text-w-sm text-w-danger">{error}</p>}
        <p className="mt-6 flex flex-wrap items-center gap-x-3 gap-y-1 border-t-2 border-dashed border-w-line pt-3 text-w-xs text-w-muted">
          <span>Type / for blocks, [[ to link a report</span>
          <span className="flex items-center gap-1"><Keycap size="sm">⌘</Keycap><Keycap size="sm">S</Keycap> saves</span>
          <span className="flex items-center gap-1"><Keycap size="sm">Esc</Keycap> {selected ? "done" : "cancel"}</span>
          <span className="ml-auto tabular-nums">{sizeLabel(byteLength(draft.body))} of 512 KB</span>
        </p>
      </article>
    </div>
  );

  const toc = headings.length > 1 && (
    <div>
      <h3 className="mb-2.5 text-w-xs font-medium text-w-muted">On this page</h3>
      <ul className="space-y-0.5 border-l-2 border-w-line">
        {headings.map((h, i) => (
          <li key={`${h.id}-${i}`}>
            <button
              type="button"
              onClick={() => jump(h.id)}
              aria-current={activeHeading === h.id ? "location" : undefined}
              className={cn(
                "-ml-0.5 block w-full truncate border-l-2 py-1.5 pr-1 text-left text-w-sm transition-colors duration-200",
                activeHeading === h.id ? "border-w-teal font-medium text-w-strong" : "border-transparent text-w-muted hover:border-w-line2 hover:text-w-text",
              )}
              style={{ paddingLeft: `${0.5 + (h.depth - 1) * 0.7}rem` }}
            >
              {h.text}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );

  const reader = report && (
    <div className="mx-auto flex w-full max-w-[72rem] gap-12 px-5 pb-20 pt-5 @xl/reader:px-10 @xl/reader:pt-6 @4xl/reader:px-14">
      <article className="mx-auto min-w-0 max-w-[72ch] flex-1">
        <header className="mb-8">
          <div className="mb-6 flex flex-wrap items-center gap-2">
            {report.folder ? (
              <nav aria-label="Folder" className="flex min-w-0 flex-1 flex-wrap items-center gap-1 text-w-sm text-w-muted">
                <FolderSimple size={14} className="shrink-0" />
                {folderParts(report.folder).map((part, i, parts) => (
                  <span key={i} className="flex items-center gap-1">
                    {i > 0 && <CaretRight size={10} weight="bold" className="text-w-faint" />}
                    <button type="button" onClick={() => pickFolder(parts.slice(0, i + 1).join("/"))} className="rounded px-0.5 transition-colors hover:text-w-text">
                      {part}
                    </button>
                  </span>
                ))}
              </nav>
            ) : (
              <span className="flex-1" />
            )}
            <div className="flex items-center gap-0.5">
              {detail.can_edit && !revision && (
                <button type="button" onClick={() => startEdit()} className={TOOL_BTN} title="Edit (E, or double-click the page)">
                  <PencilSimple size={15} />
                  <span>Edit</span>
                </button>
              )}
              <button type="button" onClick={() => void copyMarkdown()} className={TOOL_BTN} title={revision ? `Copy revision ${revision.revision} as Markdown` : "Copy the whole report as Markdown"}>
                {copiedText ? <Check size={15} weight="bold" className="text-w-ok" /> : <Copy size={15} />}
                <span>{copiedText ? "Copied" : "Copy"}</span>
              </button>
              <button type="button" onClick={() => void copyLink()} className={TOOL_BTN} title="Copy a link. Paste it in chat to share it as a card">
                {copied ? <Check size={15} weight="bold" className="text-w-ok" /> : <LinkIcon size={15} />}
                <span>{copied ? "Link copied" : "Share"}</span>
              </button>
              <button type="button" onClick={downloadMarkdown} title="Download as a Markdown file" aria-label="Download as a Markdown file" className={TOOL_ICON}>
                <DownloadSimple size={16} />
              </button>
              <button
                type="button"
                onClick={() => {
                  setRevision(null);
                  setMode(mode === "history" ? "read" : "history");
                }}
                aria-pressed={mode === "history"}
                title="History"
                aria-label="History"
                className={cn(TOOL_ICON, mode === "history" && "bg-w-sand text-w-strong")}
              >
                <ClockCounterClockwise size={16} />
              </button>
              {detail.can_edit && (
                <button
                  type="button"
                  onClick={() => void patch({ pinned: !report.pinned })}
                  aria-pressed={report.pinned}
                  title={report.pinned ? "Unpin" : "Pin"}
                  aria-label={report.pinned ? "Unpin" : "Pin"}
                  className={cn(TOOL_ICON, report.pinned && "text-w-strong")}
                >
                  <PushPinSimple size={16} weight={report.pinned ? "fill" : "regular"} />
                </button>
              )}
              {detail.can_edit && (
                <button type="button" onClick={() => void remove()} title="Delete" aria-label="Delete" className={cn(TOOL_ICON, "hover:bg-w-rose hover:text-w-danger")}>
                  <Trash size={16} />
                </button>
              )}
            </div>
          </div>
          {error && (
            <p role="alert" className="mb-4 rounded-lg border-2 border-w-danger/40 bg-w-rose px-3 py-2 text-w-sm text-w-danger">
              {error}
            </p>
          )}
          <h2 className="text-[2.1rem] font-bold leading-[1.15] tracking-[-0.02em] text-w-strong @xl/reader:text-[2.5rem]">{revision ? revision.title : report.title}</h2>
          <dl className="mt-5 grid grid-cols-[7.5rem_minmax(0,1fr)] gap-x-3 border-b-2 border-w-line pb-5 @xl/reader:grid-cols-[9rem_minmax(0,1fr)]">
            <Property icon={UserCircle} label="Author">{author(report.author_agent_id, report.author_user_id)}</Property>
            <Property icon={Clock} label="Updated">
              <RelativeTime at={report.updated_at} />
              <span className="text-w-muted">· {readMinutes(byteLength(report.body))} · revision {report.revision}</span>
            </Property>
            {!SOLO && (
              <Property icon={report.visibility === "public" ? Globe : Lock} label="Access">
                {detail.can_edit ? (
                  <button
                    type="button"
                    onClick={() => void patch({ visibility: report.visibility === "public" ? "private" : "public" })}
                    title={report.visibility === "public" ? "Make private" : "Share with everyone"}
                    className={cn(report.visibility === "public" ? PILL_INFO : PILL_NEUTRAL, "py-1 transition-colors hover:border-w-ink")}
                  >
                    {report.visibility === "public" ? <Globe size={11} weight="bold" /> : <Lock size={11} weight="bold" />}
                    {report.visibility === "public" ? "Everyone can read" : "Only you"}
                  </button>
                ) : (
                  <span className={cn(report.visibility === "public" ? PILL_INFO : PILL_NEUTRAL, "py-1")}>{report.visibility === "public" ? "Everyone can read" : "Private"}</span>
                )}
              </Property>
            )}
            {report.tags.length > 0 && (
              <Property icon={Hash} label="Tags">
                {report.tags.map((t) => (
                  <button key={t} type="button" onClick={() => setScope({ kind: "tag", tag: t })} className="rounded-md bg-w-sand px-2 py-0.5 text-w-xs text-w-text2 transition-colors hover:bg-w-line hover:text-w-text">
                    {t}
                  </button>
                ))}
              </Property>
            )}
          </dl>
        </header>
        {mode === "history" && (
          <section aria-label="Revision history" className="mb-8 rounded-xl border-2 border-w-line bg-w-card/70 p-4">
            <div className="mb-2 flex items-center justify-between gap-2">
              <h3 className="text-w-base font-semibold">History</h3>
              {revision && (
                <button type="button" onClick={() => setRevision(null)} className="text-w-xs text-w-link hover:underline">
                  Back to the current text
                </button>
              )}
            </div>
            {revisions === null ? (
              <p className="text-w-sm text-w-muted">
                <Shimmer>Reading the history</Shimmer>
              </p>
            ) : (
              <ul className="max-h-64 space-y-1 overflow-y-auto">
                {revisions.map((rv) => {
                  const current = rv.revision === report.revision;
                  const viewing = revision?.revision === rv.revision;
                  return (
                    <li key={rv.revision} className={cn("flex items-center gap-2 rounded-lg px-2 py-1.5 text-w-sm transition-colors", viewing ? "bg-w-sand ring-2 ring-inset ring-w-line2" : "hover:bg-w-sand/70")}>
                      <button
                        type="button"
                        onClick={() =>
                          current
                            ? setRevision(null)
                            : void reportsApi.revision(report.id, rv.revision).then(setRevision).catch(() => setRevision(null))
                        }
                        className="flex min-w-0 flex-1 items-center gap-2 text-left"
                      >
                        <span className="w-8 shrink-0 tabular-nums text-w-muted">r{rv.revision}</span>
                        <span className="min-w-0 flex-1 truncate">
                          {author(rv.author_agent_id, rv.author_user_id)}
                          {rv.title !== report.title && <span className="text-w-muted"> as &ldquo;{rv.title}&rdquo;</span>}
                        </span>
                        <span className="shrink-0 tabular-nums text-w-2xs text-w-muted">{sizeLabel(rv.size)}</span>
                        <RelativeTime at={rv.created_at} className="shrink-0 text-w-2xs text-w-muted" />
                      </button>
                      {current ? (
                        <span className="shrink-0 rounded-[0.3rem] border-[1.5px] border-w-ok/55 bg-w-mint px-1.5 text-w-2xs font-semibold text-w-ok">current</span>
                      ) : (
                        detail.can_edit && (
                          <button type="button" onClick={() => void restore(rv.revision)} className="shrink-0 rounded-md p-1 text-w-muted hover:bg-w-card hover:text-w-text" aria-label={`Restore revision ${rv.revision}`} title="Restore">
                            <ArrowCounterClockwise size={13} weight="bold" />
                          </button>
                        )
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
            {revision && <p className="mt-2 text-w-xs text-w-warn">Showing revision {revision.revision} from {new Date(revision.created_at).toLocaleString()}.</p>}
          </section>
        )}
        {headings.length > 2 && (
          <div className="mb-5 @4xl/reader:hidden">
            <button type="button" onClick={() => setTocOpen((v) => !v)} aria-expanded={tocOpen} className="flex items-center gap-1.5 text-w-sm font-semibold text-w-text2 hover:text-w-strong">
              <ListBullets size={14} weight="bold" /> Contents
              {tocOpen ? <CaretDown size={11} weight="bold" /> : <CaretRight size={11} weight="bold" />}
            </button>
            {tocOpen && <div className="mt-2 rounded-xl border-2 border-w-line bg-w-card/70 p-3">{toc}</div>}
          </div>
        )}
        <div
          className={PROSE}
          onDoubleClick={(e) => {
            if (!detail.can_edit || revision || (e.target as HTMLElement).closest("a,button,input,textarea")) return;
            startEdit(focusPointAt(e.clientX, e.clientY));
          }}
        >
          {body.trim() ? (
            <RunCommandContext.Provider value={reportRunner}>
              <LocalPathContext.Provider value={reportAuthor && meId && reportAuthor.owner_user_id === meId ? reportAuthor : null}>
                <div {...quoteProps(`report:${report.id}`, `Report "${revision ? revision.title : report.title}"`)}>
                  <RichText text={wikify(withoutTitle(body, revision ? revision.title : report.title), known)} handles={handles} onOpenAgent={onOpenAgent} onWiki={openWiki} renderUi={liveCard} prose />
                </div>
              </LocalPathContext.Provider>
            </RunCommandContext.Provider>
          ) : (
            detail.can_edit && !revision ? (
              <button type="button" onClick={() => startEdit()} className="text-w-muted transition-colors hover:text-w-text">
                Empty page. Click to start writing.
              </button>
            ) : (
              <p className="text-w-muted">This report is empty.</p>
            )
          )}
        </div>
        {(detail.backlinks.length > 0 || detail.outlinks.length > 0) && (
          <footer className="mt-12 grid gap-4 border-t-2 border-w-line pt-5 @xl/reader:grid-cols-2 @4xl/reader:hidden">
            <LinkList title={`Linked from (${detail.backlinks.length})`} items={detail.backlinks} onOpen={select} />
            <LinkList title={`Links to (${detail.outlinks.length})`} items={detail.outlinks} onOpen={select} />
          </footer>
        )}
      </article>
      <aside className="sticky top-6 hidden max-h-[calc(100vh-12rem)] w-56 shrink-0 self-start overflow-y-auto pt-12 @4xl/reader:block">
        {toc}
        <LinkList title={`Linked from (${detail.backlinks.length})`} items={detail.backlinks} onOpen={select} />
        {detail.backlinks.length === 0 && (
          <>
            <h3 className="mb-1.5 mt-6 text-w-xs font-medium text-w-muted">Linked from</h3>
            <p className="px-2 text-w-xs text-w-faint">No other report links here yet.</p>
          </>
        )}
        <LinkList title={`Links to (${detail.outlinks.length})`} items={detail.outlinks} onOpen={select} />
      </aside>
    </div>
  );

  return (
    <div role="dialog" aria-modal="true" aria-label="Reports" className={cn(PANEL, "pointer-events-auto flex h-full w-full flex-col overflow-hidden @container")}>
      <WindowHeader
        title="Reports"
        subtitle="Write-ups from your agents and you, linked like a notebook"
        leading={
          <>
            {readerOpen && (
              <button type="button" onClick={() => void (editing ? leaveEdit() : select(null))} aria-label="Back to the library" className={cn(ICON_BTN_MD, "@2xl:hidden")}>
                <ArrowLeft size={16} weight="bold" />
              </button>
            )}
            <button
              type="button"
              onClick={toggleLibrary}
              aria-pressed={!libraryHidden}
              aria-label={libraryHidden ? "Show the library" : "Hide the library"}
              title={libraryHidden ? "Show the library" : "Hide the library"}
              className={cn(GHOST_ICON_BTN, "hidden @2xl:grid")}
            >
              <SidebarSimple size={18} weight={libraryHidden ? "regular" : "duotone"} />
            </button>
          </>
        }
        onClose={onClose}
        actions={
          <>
            <ScopeToggle others={othersCount} className="hidden @3xl:inline-flex" />
            <button type="button" onClick={() => startNew()} className={BTN_PRIMARY}>
              <Plus size={14} weight="bold" />
              <span className="hidden @lg:inline">New report</span>
            </button>
            {sideToggle}
          </>
        }
      />
      <div className="flex min-h-0 flex-1">
        <div
          style={libraryPane.style}
          className={cn(
            "flex min-h-0 w-full flex-col overflow-hidden border-w-line2 transition-[width,opacity] duration-300 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none @2xl:shrink-0 @2xl:border-dashed",
            libraryHidden ? "@2xl:!w-0 @2xl:border-r-0 @2xl:opacity-0" : cn("@2xl:border-r-2", libraryPane.custom ? "@2xl:w-[var(--pane-w)]" : "@2xl:w-72 @5xl:w-80"),
            SIDEBAR,
            readerOpen && "hidden @2xl:flex",
          )}
          aria-hidden={libraryHidden || undefined}
          inert={libraryHidden || undefined}
        >
          <div className="shrink-0 p-3 @2xl:min-w-[16rem]">
            <FilterField
              value={query}
              onChange={setQuery}
              placeholder="Search reports"
              className="w-full"
              findTarget={!readerOpen}
              onEnter={() => shown[0] && void select(shown[0].id)}
              onArrowDown={() => listRef.current?.querySelector<HTMLButtonElement>("button[data-report]")?.focus()}
            />
          </div>
          <div className={cn("min-h-0 flex-1 overflow-y-auto px-3 pb-4 @2xl:min-w-[16rem]", NAV_GLIDE)}>
            <Glide hover=".nav-off" className={NAV_PILL} />
            {sidebar}
            {list}
          </div>
        </div>
        {!libraryHidden && <PaneResizer pane={libraryPane} edge="right" label="Resize the library" className="hidden @2xl:block" />}
        <div ref={readerRef} className={cn("min-h-0 flex-1 overflow-y-auto @container/reader", !readerOpen && "hidden @2xl:block")}>
          {mode === "edit" ? (
            editor
          ) : reader ? (
            reader
          ) : missing ? (
            <div className="grid h-full place-items-center p-6 text-center text-w-base text-w-muted">This report was deleted or is not shared with you.</div>
          ) : selected ? (
            <div className="grid h-full place-items-center p-6 text-w-base text-w-muted">
              <Shimmer>Opening the report</Shimmer>
            </div>
          ) : (
            <div className="grid h-full place-items-center p-6">
              <EmptyState
                size="lg"
                icon={Notebook}
                title={visible.length ? "Pick a report to read" : "No reports yet"}
                description="Agents write a report when their findings are too long for chat, then share it as a card. You can start one yourself too."
                action={
                  <button type="button" onClick={() => startNew()} className={BTN_PRIMARY}>
                    <Plus size={14} weight="bold" /> New report
                  </button>
                }
              />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
