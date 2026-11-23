"use client";

import "@xyflow/react/dist/base.css";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type CSSProperties, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import dynamic from "next/dynamic";
import {
  Background,
  BackgroundVariant,
  Handle,
  MarkerType,
  MiniMap,
  NodeResizer,
  NodeToolbar,
  Position,
  ReactFlow,
  ReactFlowProvider,
  applyNodeChanges,
  useReactFlow,
  useViewport,
  type Edge,
  type Node,
  type NodeChange,
  type NodeProps,
} from "@xyflow/react";
import {
  ArrowsOut,
  ArrowUpRight,
  Cards,
  ChatCircleDots,
  Check,
  ClockCounterClockwise,
  CornersOut,
  Cursor,
  Hand,
  ImageSquare,
  Link as LinkIcon,
  Lock,
  MapPin,
  MapTrifold,
  Minus,
  MonitorPlay,
  Note,
  PencilSimple,
  Phone,
  Plus,
  At,
  Hash,
  Layout,
  UsersThree,
  Quotes,
  GraduationCap,
  FolderSimple,
  Trash,
  TreeStructure,
  X,
  type Icon as PhosphorIcon,
} from "@phosphor-icons/react";
import { PRACTICE_THIS_HINT, usePracticeThis } from "./practice-this";
import { AnimatePresence, motion } from "framer-motion";
import { cn } from "@/lib/utils";
import { worldApi, type ChannelSummary, type SearchResults, type WorldAgent } from "@/lib/world";
import { canvasItemKind, canvasItemLabel, openReport, ReportError, reportsApi, wikify, type CanvasDoc, type CanvasItem, type CanvasMark, type CanvasOp, type ReportBrief, type ReportDetail, type StickyColor } from "@/lib/reports";
import { RichText } from "./chat-kit";
import { embedFromHref, EmbedView } from "./embeds";
import { Lightbox } from "./media-view";
import { UiCard } from "./lazy";
import { MAIN_TARGET, sendQuote } from "./quote";
import { RunCommandContext, useRunCommand, type RunCommand } from "./run-command";
import { BTN_PRIMARY, BTN_GHOST, BTN_DANGER_GHOST, BTN_SM, EASE, FOCUS } from "./styles";
import { POPOVER_MOTION } from "./motion";
import { useCopy } from "@/lib/copy";
import { commitMarkDelete, commitMarkDeleteOnExit, hideMarkDeletes, MARK_UNDO_MS, planMarkDelete, pruneMarkDeletes, settleMarkDelete, type MarkDelete, type MarkDeleteApi } from "@/lib/canvas-marks";
import { takeEscape } from "@/lib/escape";
import { workspaceHref } from "@/lib/feature-links";
import { MeaningIcon } from "@/lib/meaning-icons";
import { CanvasCursors } from "./canvas-cursors";
import { AgentFace } from "./channel-ui";
import { readDraft, saveDraft } from "./drafts";
import { CALL_ABOUT_EVENT, MAX_QUOTED, quoteItems } from "@/lib/canvas-quote";
import { EMBED_DRAG, embedItem, readEmbedDrag, type EmbedDrop } from "@/lib/canvas-embeds";
import { EmbedItem, EmbedPicker, EmbedStage, type EmbedHost } from "./canvas-embed";

const CanvasModel = dynamic(() => import("./canvas-model"), { ssr: false });

type Tool = "select" | "hand" | "pen" | "pin" | "sticky" | "arrow" | "thing" | "embed";
type ItemNode = Node<{ item: CanvasItem }, "item">;
type MarkNode = Node<{ marks: CanvasMark[] }, "mark">;
type CanvasNode = ItemNode | MarkNode;

const STICKY: Record<StickyColor, string> = {
  lemon: "bg-w-lemon",
  rose: "bg-w-rose",
  mint: "bg-w-mint",
  aqua: "bg-w-aqua",
  honey: "bg-w-honey/60",
  sand: "bg-w-sand",
};

const TOOLS: { tool: Tool | "image"; icon: PhosphorIcon; label: string; key: string }[] = [
  { tool: "select", icon: Cursor, label: "Select and move", key: "Esc" },
  { tool: "hand", icon: Hand, label: "Pan", key: "H" },
  { tool: "pen", icon: PencilSimple, label: "Draw", key: "P" },
  { tool: "pin", icon: MapPin, label: "Pin a mark on an item", key: "M" },
  { tool: "sticky", icon: Note, label: "Sticky note", key: "S" },
  { tool: "arrow", icon: ArrowUpRight, label: "Arrow between two items", key: "A" },
  { tool: "image", icon: ImageSquare, label: "Add an image, video or GLB model", key: "I" },
  { tool: "thing", icon: Cards, label: "Add a quest, report, to-do, message or agent as a card", key: "L" },
  { tool: "embed", icon: MonitorPlay, label: "Add a live terminal or running quest", key: "E" },
];

const SIDES: [string, Position][] = [
  ["t", Position.Top],
  ["r", Position.Right],
  ["b", Position.Bottom],
  ["l", Position.Left],
];

const FLOW_VARS = {
  "--xy-edge-stroke": "rgb(var(--w-chart-3))",
  "--xy-edge-stroke-selected": "rgb(var(--w-ink))",
  "--xy-edge-stroke-width": "2.5",
  "--xy-edge-label-background-color": "rgb(var(--w-paper))",
  "--xy-edge-label-color": "rgb(var(--w-text2))",
} as CSSProperties;

interface Ctx {
  report: { id: string; title: string };
  author: WorldAgent | undefined;
  known: (title: string) => boolean;
  onWiki: (title: string) => void;
  single: string | null;
  quoteItem: (item: CanvasItem) => void;
  linkItem: (item: CanvasItem) => void;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  runner: RunCommand | null;
  canEdit: boolean;
  openMark: string | null;
  setOpenMark: (id: string | null) => void;
  resolve: (mark: CanvasMark, resolved: boolean) => void;
  removeMark: (mark: CanvasMark) => void;
  resize: (item: CanvasItem, box: { x: number; y: number; width: number; height: number }) => void;
  openMedia: (item: CanvasItem) => void;
  embedHost: EmbedHost;
}

const CanvasCtx = createContext<Ctx | null>(null);

const useCtx = () => {
  const ctx = useContext(CanvasCtx);
  if (!ctx) throw new Error("canvas context missing");
  return ctx;
};

const upload = (id?: string) => `/api/world/uploads/${id ?? ""}`;
const reduced = () => typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function Caption({ item }: { item: CanvasItem }) {
  if (!item.caption && !item.locked) return null;
  return (
    <p className="absolute left-0 right-0 top-full mt-1.5 line-clamp-3 text-w-xs leading-snug text-w-text2">
      {item.by !== "person" && <MeaningIcon.agent size={11} weight="fill" className="mr-1 inline text-[rgb(var(--w-chart-3))]" aria-hidden />}
      {item.caption}
      {item.locked && (
        <span className="ml-1 inline-flex items-center gap-0.5 font-semibold text-w-strong">
          <Lock size={10} weight="bold" aria-hidden />
          locked
        </span>
      )}
    </p>
  );
}

function RefCard({ target }: { target: string }) {
  const { handles, onOpenAgent } = useCtx();
  const embed = embedFromHref(target);
  return embed ? <EmbedView embed={embed} card /> : <RichText text={target} handles={handles} onOpenAgent={onOpenAgent} />;
}

function ItemCard({ item }: { item: CanvasItem }) {
  const { report, author, handles, onOpenAgent } = useCtx();
  const [sent, setSent] = useState(false);
  const answer = async (text: string) => {
    if (!author) return false;
    try {
      await worldApi.send(author.id, `About report:${report.id}#${item.id} "${report.title}":\n\n${text}`);
      setSent(true);
      return true;
    } catch {
      return false;
    }
  };
  return (
    <div className="nowheel nodrag h-full w-full overflow-auto rounded-lg border-2 border-w-line2 bg-w-card p-2 text-w-sm leading-normal shadow-[0_2px_0_rgb(var(--w-line))]">
      <UiCard
        spec={item.spec}
        stateKey={`canvas:${report.id}:${item.id}`}
        handles={handles}
        onOpenAgent={onOpenAgent}
        canAnswer={!!author}
        answered={sent}
        answeredLabel={author ? `Sent to @${author.slug}` : "Sent"}
        onAnswer={answer}
      />
    </div>
  );
}

const isPreviewable = (item: CanvasItem) => item.kind === "image" || item.kind === "video";

function ImageItem({ item }: { item: CanvasItem }) {
  const { openMedia } = useCtx();
  const ref = useRef<HTMLImageElement>(null);
  useEffect(() => {
    const img = ref.current;
    if (!img) return;
    const onDouble = (e: MouseEvent) => {
      e.stopPropagation();
      openMedia(item);
    };
    img.addEventListener("dblclick", onDouble);
    return () => img.removeEventListener("dblclick", onDouble);
  }, [item, openMedia]);
  return <img ref={ref} src={upload(item.upload_id)} alt={item.caption ?? "Image"} draggable={false} className="h-full w-full cursor-zoom-in rounded-md bg-w-sand object-cover" />;
}

function ItemBody({ item }: { item: CanvasItem }) {
  const { handles, onOpenAgent, runner, known, onWiki, embedHost } = useCtx();
  switch (item.kind) {
    case "embed":
      return <EmbedItem item={item} host={embedHost} />;
    case "image":
      return <ImageItem item={item} />;
    case "video":
      return <video src={upload(item.upload_id)} controls muted playsInline preload="metadata" className="nodrag h-full w-full rounded-md bg-w-ink object-contain" />;
    case "model":
      return (
        <div className="h-full w-full overflow-hidden rounded-md border-2 border-w-line bg-w-card">
          <CanvasModel src={upload(item.upload_id)} label={item.caption ?? "3D model"} />
        </div>
      );
    case "note":
      return (
        <div className="nowheel relative h-full w-full overflow-y-auto rounded-lg border-2 border-w-line2 bg-w-card px-3.5 py-3 text-w-sm leading-relaxed text-w-text shadow-[0_2px_0_rgb(var(--w-line))]">
          {item.by !== "person" && <MeaningIcon.agent size={12} weight="fill" className="absolute right-2.5 top-2.5 text-[rgb(var(--w-chart-3))]" aria-label="Written by the agent" />}
          <RichText text={wikify(item.text ?? "", known)} handles={handles} onOpenAgent={onOpenAgent} onWiki={onWiki} />
        </div>
      );
    case "sticky":
      return (
        <div
          className={cn(
            "nowheel h-full w-full -rotate-1 overflow-y-auto rounded-md border-2 border-w-ink/70 px-3.5 py-3 text-w-base italic leading-snug text-w-strong shadow-[0_3px_0_rgb(var(--w-ink)/0.6)] motion-reduce:rotate-0",
            STICKY[item.color ?? "rose"],
          )}
        >
          <RichText text={wikify(item.text ?? "", known)} handles={handles} onOpenAgent={onOpenAgent} onWiki={onWiki} />
        </div>
      );
    case "ui":
      return <ItemCard item={item} />;
    case "ref":
      return (
        <div className="nowheel h-full w-full overflow-hidden">
          <RefCard target={item.target ?? ""} />
        </div>
      );
    case "run":
      return (
        <div className="nowheel nodrag h-full w-full overflow-y-auto rounded-lg border-2 border-w-line2 bg-w-card p-2 text-w-sm">
          <RunCommandContext.Provider value={runner}>
            <RichText text={`\`\`\`bash\n${item.command ?? ""}\n\`\`\``} handles={handles} onOpenAgent={onOpenAgent} />
          </RunCommandContext.Provider>
        </div>
      );
    case "frame":
      return (
        <div className="h-full w-full rounded-xl border-2 border-dashed border-w-line2 bg-w-sand/30">
          {item.caption && <p className="absolute -top-7 left-1 truncate text-w-sm font-semibold text-w-text2">{item.caption}</p>}
        </div>
      );
    case "ink": {
      const points = (item.points ?? []).map(([x, y]) => `${x},${y}`).join(" ");
      return (
        <svg width={item.w} height={item.h} className="overflow-visible text-[rgb(var(--w-chart-5))]" aria-label="Drawing">
          <polyline points={points} fill="none" stroke="currentColor" strokeWidth={3} strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      );
    }
  }
}

function ItemView({ data, selected }: NodeProps<ItemNode>) {
  const { canEdit, resize, single, quoteItem, linkItem, openMedia } = useCtx();
  const item = data.item;
  const media = item.kind === "image" || item.kind === "video" || item.kind === "model";
  return (
    <div className={cn("relative h-full w-full", media && selected && "rounded-md ring-2 ring-w-ink ring-offset-2 ring-offset-w-paper")}>
      <NodeToolbar isVisible={single === item.id && item.kind !== "ink"} position={Position.Top} offset={item.kind === "frame" ? 34 : 10}>
        <div className="flex items-center gap-0.5 rounded-lg border-2 border-w-ink bg-w-paper p-0.5 shadow-[0_3px_0_rgb(var(--w-ink))]">
          <span className="px-1.5 text-w-2xs font-semibold text-w-muted">{canvasItemKind(item)}</span>
          {isPreviewable(item) && <ClusterButton icon={ArrowsOut} label="Open larger" onClick={() => openMedia(item)} />}
          <ClusterButton icon={Quotes} label="Quote in chat" onClick={() => quoteItem(item)} />
          <ClusterButton icon={LinkIcon} label="Copy a link to this item" onClick={() => linkItem(item)} />
        </div>
      </NodeToolbar>
      <NodeResizer
        isVisible={canEdit && selected && item.kind !== "ink"}
        minWidth={40}
        minHeight={32}
        lineClassName="!border-w-teal"
        handleClassName="!h-2.5 !w-2.5 !rounded-sm !border-2 !border-w-ink !bg-w-paper"
        onResizeEnd={(_, box) => resize(item, box)}
      />
      {SIDES.map(([side, position]) => (
        <Handle key={`t${side}`} id={`t${side}`} type="target" position={position} isConnectable={false} className="!invisible" />
      ))}
      <ItemBody item={item} />
      {item.kind !== "frame" && item.kind !== "note" && item.kind !== "embed" && <Caption item={item} />}
      {item.kind === "note" && item.caption && <p className="absolute left-0 right-0 top-full mt-1.5 text-w-xs text-w-text2">{item.caption}</p>}
      {SIDES.map(([side, position]) => (
        <Handle key={`s${side}`} id={`s${side}`} type="source" position={position} isConnectable={false} className="!invisible" />
      ))}
    </div>
  );
}

function MarkView({ data }: NodeProps<MarkNode>) {
  const { openMark, setOpenMark, resolve, removeMark, canEdit } = useCtx();
  const { zoom } = useViewport();
  const marks = data.marks;
  const open = marks.filter((m) => m.status === "open");
  const shown = marks.some((m) => m.id === openMark);
  return (
    <div className="nodrag relative">
      <button
        type="button"
        onClick={() => setOpenMark(shown ? null : marks[0].id)}
        aria-label={`${open.length} open marks`}
        className={cn(
          "grid h-7 w-7 -translate-x-1/2 -translate-y-full place-items-center rounded-md border-2 border-w-ink shadow-[0_2px_0_rgb(var(--w-ink))]",
          EASE,
          FOCUS,
          open.length ? "bg-w-alert text-w-onalert" : "bg-w-paper text-w-faint",
        )}
      >
        {open.length > 1 ? <span className="text-w-2xs font-bold">{open.length}</span> : open.length ? <MapPin size={14} weight="fill" /> : <Check size={13} weight="bold" />}
      </button>
      <div className="absolute left-0 top-1 z-10 origin-top-left" style={{ transform: `scale(${1 / zoom})` }}>
        <AnimatePresence>
          {shown && (
            <motion.div {...POPOVER_MOTION} className="nowheel w-72 origin-top-left space-y-2 rounded-lg border-2 border-w-ink bg-w-paper p-3 shadow-[0_3px_0_rgb(var(--w-ink))]">
              {marks.map((m) => (
                <div key={m.id} className="space-y-1 border-b border-w-line pb-2 last:border-0 last:pb-0">
                  <p className={cn("whitespace-pre-wrap text-w-sm leading-snug", m.status === "open" ? "text-w-strong" : "text-w-muted line-through")}>{m.body}</p>
                  {m.reply && (
                    <p className="flex gap-1 text-w-xs text-w-text2">
                      <MeaningIcon.reply size={11} weight="bold" className="mt-0.5 shrink-0 text-[rgb(var(--w-chart-3))]" aria-hidden />
                      {m.reply}
                    </p>
                  )}
                  {canEdit && (
                    <div className="-ml-2.5 flex items-center gap-3">
                      <button type="button" onClick={() => resolve(m, m.status === "open")} className={cn(BTN_GHOST, "h-11 text-w-xs sm:h-7")}>
                        {m.status === "open" ? <Check size={12} weight="bold" /> : <ClockCounterClockwise size={12} />}
                        {m.status === "open" ? "Resolve" : "Reopen"}
                      </button>
                      <button type="button" onClick={() => removeMark(m)} aria-label={m.kind === "sticky" ? "Delete this sticky note" : "Delete this mark"} className={cn(BTN_DANGER_GHOST, "h-11 text-w-xs sm:h-7")}>
                        <Trash size={12} weight="bold" aria-hidden />
                        Delete
                      </button>
                    </div>
                  )}
                </div>
              ))}
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </div>
  );
}

const nodeTypes = { item: ItemView, mark: MarkView };

function sides(from: CanvasItem | undefined, to: CanvasItem | undefined): [string, string] {
  if (!from || !to) return ["r", "l"];
  const dx = to.x + to.w / 2 - (from.x + from.w / 2);
  const dy = to.y + to.h / 2 - (from.y + from.h / 2);
  if (Math.abs(dx) * 0.75 >= Math.abs(dy)) return dx >= 0 ? ["r", "l"] : ["l", "r"];
  return dy >= 0 ? ["b", "t"] : ["t", "b"];
}

function insideFrames(items: CanvasItem[], frames: CanvasItem[], moving: Set<string>): CanvasItem[] {
  return items.filter((i) => !moving.has(i.id) && !i.locked && frames.some((f) => i.x >= f.x && i.y >= f.y && i.x + i.w <= f.x + f.w && i.y + i.h <= f.y + f.h));
}

function buildNodes(doc: CanvasDoc, marks: CanvasMark[], selected: Set<string>, canEdit: boolean): CanvasNode[] {
  const byId = new Map(doc.items.map((i) => [i.id, i]));
  const items: CanvasNode[] = doc.items.map((item) => ({
    id: item.id,
    type: "item",
    position: { x: item.x, y: item.y },
    width: item.w,
    height: item.h,
    data: { item },
    zIndex: item.kind === "frame" ? -1 : 0,
    selected: selected.has(item.id),
    draggable: canEdit,
  }));
  const groups = new Map<string, CanvasMark[]>();
  for (const m of marks) {
    const key = m.item_id && byId.has(m.item_id) ? `${m.item_id}:${Math.round(m.x / 24)}:${Math.round(m.y / 24)}` : `free:${m.id}`;
    groups.set(key, [...(groups.get(key) ?? []), m]);
  }
  const pins: CanvasNode[] = [...groups.entries()].map(([key, list]) => {
    const m = list[0];
    const host = m.item_id ? byId.get(m.item_id) : undefined;
    const at = host ? { x: host.x + (m.kind === "sticky" ? host.w - 6 : m.x), y: host.y + (m.kind === "sticky" ? 6 : m.y) } : { x: m.x, y: m.y };
    return { id: `mark:${key}`, type: "mark", position: at, data: { marks: list }, draggable: false, selectable: false, zIndex: 10 };
  });
  return [...items, ...pins];
}

interface Composer {
  kind: "pin" | "sticky";
  screen: { x: number; y: number };
  flow: { x: number; y: number };
  item: CanvasItem | null;
}

interface BoardProps {
  detail: ReportDetail;
  agents: WorldAgent[];
  meId: string | null;
  handles: Map<string, WorldAgent>;
  focus?: { item?: string | null } | null;
  known: (title: string) => boolean;
  onWiki: (title: string) => void;
  onOpenAgent: (id: string) => void;
  onChanged: () => void;
  onHistory: () => void;
  onRename: (title: string) => Promise<void>;
  folders: string[];
  onMoveFolder: (folder: string) => Promise<void>;
  onWorkspace?: () => void;
  inWorkspace?: boolean;
  workspaceChannel?: string | null;
  onShowChannel?: (id: string) => void;
}

function Board({ detail, agents, meId, handles, focus, known, onWiki, onOpenAgent, onChanged, onHistory, onRename, folders, onMoveFolder, onWorkspace, inWorkspace = false, workspaceChannel = null, onShowChannel }: BoardProps) {
  const report = detail.report;
  const canEdit = detail.can_edit;
  const flow = useReactFlow<CanvasNode, Edge>();
  const { zoom } = useViewport();
  const wrap = useRef<HTMLDivElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const folderButton = useRef<HTMLButtonElement>(null);
  const [rawDoc, setDoc] = useState<CanvasDoc>(() => report.canvas ?? { items: [], edges: [] });
  const [rawMarks, setMarks] = useState<CanvasMark[]>(detail.marks);
  const [deletes, setDeletes] = useState<Map<string, MarkDelete>>(() => new Map());
  const [undoing, setUndoing] = useState<MarkDelete | null>(null);
  const { marks, doc } = useMemo(() => hideMarkDeletes(rawMarks, rawDoc, deletes.values()), [rawMarks, rawDoc, deletes]);
  const [tool, setTool] = useState<Tool>("select");
  const [nodes, setNodes] = useState<CanvasNode[]>([]);
  const [openMark, setOpenMark] = useState<string | null>(null);
  const [composer, setComposer] = useState<Composer | null>(null);
  const [draftText, setDraftText] = useState("");
  const [arrowFrom, setArrowFrom] = useState<string | null>(null);
  const [stroke, setStroke] = useState<{ screen: [number, number][]; flow: [number, number][] } | null>(null);
  const [minimap, setMinimap] = useState(() => typeof window === "undefined" || window.matchMedia("(min-width: 640px)").matches);
  const [marksOpen, setMarksOpen] = useState(false);
  const [linksOpen, setLinksOpen] = useState(false);
  const [single, setSingle] = useState<string | null>(null);
  const [picked, setPicked] = useState<string[]>([]);
  const [note, setNote] = useState<string | null>(null);
  const [naming, setNaming] = useState<string | null>(null);
  const [foldering, setFoldering] = useState<string | null>(null);
  const [folderError, setFolderError] = useState<string | null>(null);
  const [movingFolder, setMovingFolder] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [viewing, setViewing] = useState<string | null>(null);
  const [copied, copy] = useCopy();
  const dragging = useRef(false);
  const riders = useRef<{ anchor: { id: string; x: number; y: number }; starts: Map<string, { x: number; y: number }>; pins: Map<string, { x: number; y: number }> } | null>(null);
  const selected = useRef(new Set<string>());

  useEffect(() => {
    if (dragging.current) return;
    setDoc(report.canvas ?? { items: [], edges: [] });
  }, [report.canvas]);
  useEffect(() => setMarks(detail.marks), [detail.marks]);
  useEffect(() => setDeletes((map) => pruneMarkDeletes(map, detail.marks, report.canvas ?? { items: [], edges: [] })), [detail.marks, report.canvas]);
  useEffect(() => {
    if (!dragging.current) setNodes(buildNodes(doc, marks, selected.current, canEdit));
  }, [doc, marks, canEdit]);

  const author = report.author_agent_id ? agents.find((a) => a.id === report.author_agent_id) : undefined;
  const practice = usePracticeThis(report.id);
  const ownCanvas = meId !== null && report.owner_user_id === meId;
  useEffect(() => {
    if (practice.error) setError(practice.error);
  }, [practice.error]);
  const runner = useRunCommand(author, meId, { where: author ? (author.runner_name ? `on ${author.runner_name}, output goes to ${author.name}'s chat` : `on ${author.name}'s device, output goes to its chat`) : undefined });

  const change = useCallback(
    async (ops: CanvasOp[]) => {
      setError(null);
      try {
        const next = await reportsApi.changeCanvas(report.id, ops);
        if (next.canvas) setDoc(next.canvas);
      } catch (e) {
        setError(e instanceof Error && e.message ? e.message : "That change did not save. Try again.");
        setDoc(report.canvas ?? { items: [], edges: [] });
      }
    },
    [report.id, report.canvas],
  );

  const resolve = useCallback(
    async (mark: CanvasMark, resolved: boolean) => {
      try {
        const saved = await reportsApi.setMark(report.id, mark.id, resolved);
        setMarks((list) => list.map((m) => (m.id === saved.id ? saved : m)));
      } catch {
        setError("That mark did not update. Try again.");
      }
    },
    [report.id],
  );

  const mounted = useRef(true);
  const undoTimer = useRef<number | null>(null);
  const undoRef = useRef<MarkDelete | null>(null);

  const settleDelete = useCallback((id: string, ok: boolean) => setDeletes((map) => settleMarkDelete(map, id, ok)), []);

  const commitDelete = useCallback(
    async (p: MarkDelete, how: "now" | "unmount" | "exit" = "now") => {
      const keepalive = how !== "now";
      const api: MarkDeleteApi = {
        removeMark: () => reportsApi.removeMark(report.id, p.mark.id, { keepalive }),
        changeCanvas: async (ops) => (await reportsApi.changeCanvas(report.id, ops, { keepalive })).canvas,
        isGone: (e) => e instanceof ReportError && e.status === 404,
      };
      if (how === "exit") return commitMarkDeleteOnExit(p, api);
      const result = await commitMarkDelete(p, api);
      if (!mounted.current) return;
      settleDelete(p.mark.id, result.ok);
      if (result.doc) setDoc(result.doc);
      if (result.ok) {
        setMarks((list) => list.filter((m) => m.id !== p.mark.id));
        onChanged();
      } else {
        setError(p.item ? "That sticky note was not deleted. Try again." : "That mark was not deleted. Try again.");
      }
    },
    [report.id, onChanged, settleDelete],
  );

  const takeUndo = () => {
    if (undoTimer.current !== null) window.clearTimeout(undoTimer.current);
    undoTimer.current = null;
    const p = undoRef.current;
    undoRef.current = null;
    if (mounted.current) setUndoing(null);
    return p;
  };

  const flushUndo = (how: "now" | "unmount" | "exit" = "now") => {
    const p = takeUndo();
    if (p) void commitDelete(p, how);
  };
  const flushRef = useRef(flushUndo);
  flushRef.current = flushUndo;

  useEffect(() => {
    mounted.current = true;
    const onExit = () => flushRef.current("exit");
    window.addEventListener("pagehide", onExit);
    return () => {
      window.removeEventListener("pagehide", onExit);
      mounted.current = false;
      flushRef.current("unmount");
    };
  }, []);

  const removeMark = (mark: CanvasMark) => {
    flushUndo();
    const p = planMarkDelete(mark, rawDoc);
    setError(null);
    setOpenMark(null);
    setDeletes((map) => new Map(map).set(mark.id, p));
    undoRef.current = p;
    setUndoing(p);
    undoTimer.current = window.setTimeout(() => flushRef.current(), MARK_UNDO_MS);
  };

  const undoDelete = () => {
    const p = takeUndo();
    if (p) settleDelete(p.mark.id, false);
  };

  const resize = useCallback(
    (item: CanvasItem, box: { x: number; y: number; width: number; height: number }) =>
      void change([{ op: "update", items: [{ id: item.id, x: Math.round(box.x), y: Math.round(box.y), w: Math.round(box.width), h: Math.round(box.height) }] }]),
    [change],
  );

  const edges: Edge[] = useMemo(() => {
    const byId = new Map(doc.items.map((i) => [i.id, i]));
    return doc.edges.map((e) => {
      const [out, into] = sides(byId.get(e.from), byId.get(e.to));
      return {
        id: e.id,
        source: e.from,
        target: e.to,
        sourceHandle: `s${out}`,
        targetHandle: `t${into}`,
        label: e.label,
        deletable: canEdit,
        markerEnd: { type: MarkerType.ArrowClosed, width: 18, height: 18, color: "rgb(var(--w-chart-3))" },
        labelStyle: { fill: "rgb(var(--w-text2))", fontSize: "0.75rem" },
        labelBgStyle: { fill: "rgb(var(--w-paper))" },
        labelBgPadding: [6, 3] as [number, number],
        labelBgBorderRadius: 4,
      };
    });
  }, [doc.items, doc.edges, canEdit]);

  const onNodesChange = useCallback((changes: NodeChange<CanvasNode>[]) => {
    setNodes((list) => {
      const next = applyNodeChanges(changes, list);
      selected.current = new Set(next.filter((n) => n.type === "item" && n.selected).map((n) => n.id));
      setSingle(selected.current.size === 1 ? [...selected.current][0] : null);
      const many = selected.current.size > 0 ? [...selected.current] : [];
      setPicked((was) => (was.length === many.length && was.every((id, i) => id === many[i]) ? was : many));
      return next;
    });
  }, []);

  const openMarks = marks.filter((m) => m.status === "open").length;

  const flowPoint = (e: { clientX: number; clientY: number }) => flow.screenToFlowPosition({ x: e.clientX, y: e.clientY });
  const localPoint = (e: { clientX: number; clientY: number }) => {
    const box = wrap.current?.getBoundingClientRect();
    return { x: e.clientX - (box?.left ?? 0), y: e.clientY - (box?.top ?? 0) };
  };

  const startComposer = (kind: "pin" | "sticky", e: ReactMouseEvent, item: CanvasItem | null) => {
    setDraftText("");
    setComposer({ kind, screen: localPoint(e), flow: flowPoint(e), item });
  };

  const submitComposer = async () => {
    const text = draftText.trim();
    if (!composer || !text || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (composer.kind === "pin") {
        const item = composer.item;
        const mark = await reportsApi.addMark(report.id, {
          item_id: item?.id ?? null,
          x: Math.round(item ? composer.flow.x - item.x : composer.flow.x),
          y: Math.round(item ? composer.flow.y - item.y : composer.flow.y),
          kind: "pin",
          body: text,
        });
        setMarks((list) => [...list, mark]);
      } else {
        const id = `s${Math.random().toString(16).slice(2, 10)}`;
        const next = await reportsApi.changeCanvas(report.id, [
          { op: "add", items: [{ id, kind: "sticky", text, color: "rose", x: Math.round(composer.flow.x), y: Math.round(composer.flow.y), w: 220, h: 180 }] },
        ]);
        if (next.canvas) setDoc(next.canvas);
        const mark = await reportsApi.addMark(report.id, { item_id: id, x: 0, y: 0, kind: "sticky", body: text });
        setMarks((list) => [...list, mark]);
      }
      setComposer(null);
      setTool("select");
      onChanged();
    } catch (e) {
      setError(e instanceof Error && e.message ? e.message : "That did not save. Try again.");
    } finally {
      setBusy(false);
    }
  };

  const addFiles = async (files: FileList | null) => {
    if (!files?.length) return;
    const box = wrap.current?.getBoundingClientRect();
    const center = flow.screenToFlowPosition({ x: (box?.left ?? 0) + (box?.width ?? 0) / 2, y: (box?.top ?? 0) + (box?.height ?? 0) / 2 });
    setBusy(true);
    setError(null);
    try {
      const items: Partial<CanvasItem>[] = [];
      for (const [i, file] of [...files].slice(0, 8).entries()) {
        const glb = file.name.toLowerCase().endsWith(".glb");
        const typed = glb ? new Blob([file], { type: "model/gltf-binary" }) : file;
        const saved = await worldApi.upload(typed, file.name);
        const kind = saved.mime.startsWith("image/") ? "image" : saved.mime.startsWith("video/") ? "video" : "model";
        items.push({ kind, upload_id: saved.id, caption: file.name.replace(/\.[^.]+$/, ""), x: Math.round(center.x + i * 360 - 160), y: Math.round(center.y - 120) });
      }
      await change([{ op: "add", items }]);
    } catch {
      setError("That file could not be added. Images, videos and GLB models up to 10 MB work.");
    } finally {
      setBusy(false);
      if (fileInput.current) fileInput.current.value = "";
    }
  };

  const viewCenter = () => {
    const box = wrap.current?.getBoundingClientRect();
    return flow.screenToFlowPosition({ x: (box?.left ?? 0) + (box?.width ?? 0) / 2, y: (box?.top ?? 0) + (box?.height ?? 0) / 2 });
  };

  const addEmbeds = async (drops: EmbedDrop[], at?: { x: number; y: number }) => {
    if (!drops.length) return;
    const center = at ?? viewCenter();
    await change([{ op: "add", items: drops.map((drop, i) => embedItem(drop, center, i)) }]);
    setTool("select");
    onChanged();
  };

  const dropEmbed = (e: React.DragEvent) => {
    if (!canEdit || !e.dataTransfer.types.includes(EMBED_DRAG)) return;
    e.preventDefault();
    const drop = readEmbedDrag(e.dataTransfer.getData(EMBED_DRAG));
    if (!drop) return;
    void addEmbeds([drop], flow.screenToFlowPosition({ x: e.clientX, y: e.clientY }));
  };

  const overEmbed = (e: React.DragEvent) => {
    if (!canEdit || !e.dataTransfer.types.includes(EMBED_DRAG)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = e.dataTransfer.effectAllowed === "move" ? "move" : "copy";
  };

  const addThings = async (targets: string[]) => {
    if (!targets.length) return;
    const center = viewCenter();
    await change([{ op: "add", items: targets.map((target, i) => ({ kind: "ref" as const, target, x: Math.round(center.x - 150 + i * 330), y: Math.round(center.y - 48) })) }]);
    setTool("select");
    onChanged();
  };

  const flash = (text: string) => {
    setNote(text);
    window.setTimeout(() => setNote((cur) => (cur === text ? null : cur)), 2400);
  };

  const quoteItem = (item: CanvasItem) => {
    const label = canvasItemLabel(item);
    sendQuote(MAIN_TARGET, {
      text: (label || canvasItemKind(item)).slice(0, 2000),
      label: `${canvasItemKind(item)} in workspace "${report.title}"`.slice(0, 120),
      source: `report:${report.id}#${item.id}`,
    });
  };

  const chosen = picked.map((id) => doc.items.find((i) => i.id === id)).filter((i): i is CanvasItem => Boolean(i));
  const pickedQuote = () => quoteItems(report, chosen.map((i) => ({ id: i.id, kind: canvasItemKind(i), label: canvasItemLabel(i) })));
  const guide: GuideActions = {
    toChat: () => {
      const quote = pickedQuote();
      if (quote) sendQuote(MAIN_TARGET, quote);
    },
    toChannel: (id) => {
      const quote = pickedQuote();
      if (!quote) return;
      sendQuote(`channel:${id}`, quote);
      if (inWorkspace && onShowChannel) onShowChannel(id);
      else window.dispatchEvent(new CustomEvent("world:open-channel", { detail: { id } }));
    },
    askAgent: (agent) => {
      const quote = pickedQuote();
      if (!quote) return;
      if (inWorkspace && workspaceChannel) {
        sendQuote(`channel:${workspaceChannel}`, quote);
        const key = `channel:${workspaceChannel}`;
        const draft = readDraft(key).trim();
        const mention = `@${agent.slug}`;
        saveDraft(key, draft.startsWith(mention) ? `${draft} ` : `${mention} ${draft}`.trimEnd() + " ");
        onShowChannel?.(workspaceChannel);
      } else {
        sendQuote(`agent:${agent.id}`, quote);
        onOpenAgent(agent.id);
      }
    },
  };

  const callAbout = () => window.dispatchEvent(new CustomEvent(CALL_ABOUT_EVENT, { detail: { about: `report:${report.id}` } }));

  const quoteCanvas = () =>
    sendQuote(MAIN_TARGET, {
      text: `Workspace "${report.title}" with ${doc.items.length} ${doc.items.length === 1 ? "item" : "items"}`,
      label: `Workspace "${report.title}"`.slice(0, 120),
      source: `report:${report.id}`,
    });

  const closeFolder = () => {
    setFoldering(null);
    setFolderError(null);
    window.requestAnimationFrame(() => folderButton.current?.focus());
  };

  const moveFolder = async () => {
    if (foldering === null || movingFolder) return;
    const folder = foldering.trim();
    if (folder === report.folder) {
      closeFolder();
      return;
    }
    setMovingFolder(true);
    setFolderError(null);
    try {
      await onMoveFolder(folder);
      closeFolder();
    } catch {
      setFolderError("The workspace did not move. Try again.");
    } finally {
      setMovingFolder(false);
    }
  };

  const linkItem = (item: CanvasItem) => {
    copy(workspaceHref(window.location.origin, report.id, item.id));
    flash("Link to this item copied");
  };

  const pick = (next: Tool | "image") => {
    if (next === "image") {
      fileInput.current?.click();
      return;
    }
    setTool(next);
    setArrowFrom(null);
    setComposer(null);
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.closest("input,textarea,[contenteditable=true]") || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "Escape" && (composer || tool !== "select" || openMark)) {
        takeEscape(e);
        e.stopPropagation();
        setComposer(null);
        setOpenMark(null);
        setArrowFrom(null);
        setTool("select");
        return;
      }
      const hit = TOOLS.find((t) => t.key.length === 1 && t.key.toLowerCase() === e.key.toLowerCase());
      if (!hit || (!canEdit && hit.tool !== "hand")) return;
      e.preventDefault();
      e.stopPropagation();
      pick(hit.tool);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const fit = () => void flow.fitView({ padding: 0.15, duration: reduced() ? 0 : 260 });
  const openMedia = useCallback((item: CanvasItem) => setViewing(item.id), []);
  const closeMedia = useCallback(() => setViewing(null), []);
  const focused = useRef<unknown>(null);
  useEffect(() => {
    const id = focus?.item;
    if (!id || focused.current === focus || !nodes.length) return;
    const item = doc.items.find((i) => i.id === id);
    if (!item) return;
    focused.current = focus;
    selected.current = new Set([id]);
    setSingle(id);
    setPicked([]);
    setNodes((list) => list.map((n) => ({ ...n, selected: n.id === id })));
    window.setTimeout(() => void flow.setCenter(item.x + item.w / 2, item.y + item.h / 2, { zoom: Math.max(flow.getZoom(), 0.9), duration: reduced() ? 0 : 320 }), 450);
  }, [focus, nodes.length, doc.items, flow]);
  const focusItem = (id: string | null) => {
    const item = doc.items.find((i) => i.id === id);
    if (item) void flow.setCenter(item.x + item.w / 2, item.y + item.h / 2, { zoom: Math.max(zoom, 0.8), duration: reduced() ? 0 : 260 });
  };

  const penDown = (e: ReactPointerEvent) => {
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    const p = flowPoint(e);
    const s = localPoint(e);
    setStroke({ screen: [[s.x, s.y]], flow: [[p.x, p.y]] });
  };
  const penMove = (e: ReactPointerEvent) => {
    if (!stroke) return;
    const s = localPoint(e);
    const [lx, ly] = stroke.screen[stroke.screen.length - 1];
    if (Math.hypot(s.x - lx, s.y - ly) < 3 || stroke.flow.length >= 1900) return;
    const p = flowPoint(e);
    setStroke({ screen: [...stroke.screen, [s.x, s.y]], flow: [...stroke.flow, [Math.round(p.x * 10) / 10, Math.round(p.y * 10) / 10]] });
  };
  const penUp = () => {
    const done = stroke;
    setStroke(null);
    if (done && done.flow.length > 1) void change([{ op: "add", items: [{ kind: "ink", points: done.flow }] }]);
  };

  const embedHost = useMemo<EmbedHost>(
    () => ({ reportId: report.id, ownerId: report.owner_user_id, meId, canEdit, remove: (item: CanvasItem) => void change([{ op: "remove", ids: [item.id] }]) }),
    [report.id, report.owner_user_id, meId, canEdit, change],
  );

  const ctx: Ctx = {
    report: { id: report.id, title: report.title },
    author,
    known,
    onWiki,
    single,
    quoteItem,
    linkItem,
    handles,
    onOpenAgent,
    runner,
    canEdit,
    openMark,
    setOpenMark,
    resolve,
    removeMark,
    resize,
    openMedia,
    embedHost,
  };
  const media = doc.items.filter(isPreviewable).map((i) => ({ id: i.id, mime: i.kind === "video" ? "video/mp4" : "image/png", name: i.caption ?? canvasItemKind(i), url: upload(i.upload_id) }));
  const viewingAt = media.findIndex((m) => m.id === viewing);
  const links: { label: string; list: ReportBrief[] }[] = [
    { label: "Linked from", list: detail.backlinks },
    { label: "Links to", list: detail.outlinks },
  ];
  const tools = canEdit ? TOOLS : TOOLS.filter((t) => t.tool === "select" || t.tool === "hand");

  return (
    <CanvasCtx.Provider value={ctx}>
      <EmbedStage items={doc.items} host={embedHost}>
      <div ref={wrap} style={FLOW_VARS} onDragOver={overEmbed} onDrop={dropEmbed} className={cn("@container/board relative h-full w-full bg-w-paper", tool === "pin" || tool === "sticky" || tool === "arrow" ? "[&_.react-flow__pane]:cursor-crosshair" : "")}>
        <ReactFlow<CanvasNode, Edge>
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          onNodesChange={onNodesChange}
          onNodeDragStart={(_, __, dragged) => {
            dragging.current = true;
            const moving = new Set(dragged.map((n) => n.id));
            const frames = doc.items.filter((i) => i.kind === "frame" && moving.has(i.id));
            const inside = insideFrames(doc.items, frames, moving);
            const carried = new Set([...moving, ...inside.map((i) => i.id)]);
            const pins = nodes.filter((n) => n.type === "mark" && (n as MarkNode).data.marks.some((m) => m.item_id && carried.has(m.item_id)));
            const anchor = dragged[0];
            riders.current =
              anchor && (inside.length || pins.length)
                ? {
                    anchor: { id: anchor.id, x: anchor.position.x, y: anchor.position.y },
                    starts: new Map(inside.map((i) => [i.id, { x: i.x, y: i.y }])),
                    pins: new Map(pins.map((n) => [n.id, { ...n.position }])),
                  }
                : null;
          }}
          onNodeDrag={(_, __, dragged) => {
            const ride = riders.current;
            const lead = ride && dragged.find((n) => n.id === ride.anchor.id);
            if (!ride || !lead) return;
            const dx = lead.position.x - ride.anchor.x;
            const dy = lead.position.y - ride.anchor.y;
            setNodes((list) =>
              list.map((n) => {
                const start = ride.starts.get(n.id) ?? ride.pins.get(n.id);
                return start ? { ...n, position: { x: start.x + dx, y: start.y + dy } } : n;
              }),
            );
          }}
          onNodeDragStop={(_, __, moved) => {
            dragging.current = false;
            const ride = riders.current;
            riders.current = null;
            const items = moved.filter((n) => n.type === "item").map((n) => ({ id: n.id, x: Math.round(n.position.x), y: Math.round(n.position.y) }));
            const lead = ride && moved.find((n) => n.id === ride.anchor.id);
            if (ride && lead) {
              const dx = lead.position.x - ride.anchor.x;
              const dy = lead.position.y - ride.anchor.y;
              ride.starts.forEach((start, id) => items.push({ id, x: Math.round(start.x + dx), y: Math.round(start.y + dy) }));
            }
            if (items.length) void change([{ op: "update", items }]);
          }}
          onNodesDelete={(gone) => {
            const ids = gone.filter((n) => n.type === "item").map((n) => n.id);
            if (ids.length && canEdit) void change([{ op: "remove", ids }]);
          }}
          onEdgesDelete={(gone) => canEdit && gone.length > 0 && void change([{ op: "disconnect", ids: gone.map((e) => e.id) }])}
          onPaneClick={(e) => {
            setOpenMark(null);
            if (tool === "pin" || tool === "sticky") startComposer(tool, e, null);
          }}
          onNodeClick={(e, node) => {
            if (node.type !== "item") return;
            const item = (node as ItemNode).data.item;
            if (tool === "pin") startComposer("pin", e, item);
            if (tool === "sticky") startComposer("sticky", e, null);
            if (tool === "arrow") {
              if (!arrowFrom) setArrowFrom(item.id);
              else if (arrowFrom !== item.id) {
                void change([{ op: "connect", edges: [{ from: arrowFrom, to: item.id }] }]);
                setArrowFrom(null);
              }
            }
          }}
          nodesDraggable={canEdit && tool === "select"}
          nodesConnectable={false}
          elementsSelectable={tool === "select"}
          panOnDrag={tool === "pen" ? false : true}
          deleteKeyCode={canEdit ? ["Backspace", "Delete"] : null}
          minZoom={0.05}
          maxZoom={4}
          fitView
          fitViewOptions={{ padding: 0.15 }}
          onlyRenderVisibleElements
          proOptions={{ hideAttribution: true }}
        >
          <Background variant={BackgroundVariant.Dots} gap={22} size={1.4} className="[&_circle]:fill-w-line2" />
          <CanvasCursors reportId={report.id} items={doc.items} agents={agents} meId={meId} />
          {minimap && (
            <MiniMap
              pannable
              zoomable
              position="bottom-right"
              bgColor="rgb(var(--w-card))"
              maskColor="rgb(var(--w-scrim) / 0.12)"
              className="!bottom-28 !right-3 !m-0 overflow-hidden @3xl/board:!bottom-14 rounded-lg border-2 border-w-ink"
              nodeClassName={(n) => ((n as CanvasNode).type === "mark" ? "fill-w-alert" : "fill-w-line2")}
              nodeBorderRadius={4}
            />
          )}
        </ReactFlow>

        {tool === "pen" && (
          <div className="absolute inset-0 z-10 cursor-crosshair touch-none" onPointerDown={penDown} onPointerMove={penMove} onPointerUp={penUp} onPointerCancel={penUp}>
            {stroke && (
              <svg className="pointer-events-none absolute inset-0 h-full w-full text-[rgb(var(--w-chart-5))]">
                <polyline points={stroke.screen.map(([x, y]) => `${x},${y}`).join(" ")} fill="none" stroke="currentColor" strokeWidth={3 * zoom} strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            )}
          </div>
        )}

        <div className="pointer-events-none absolute inset-x-3 top-3 z-20 flex items-start justify-between gap-2">
          <div className="pointer-events-auto relative flex min-w-0 items-center gap-2 rounded-lg border-2 border-w-ink bg-w-paper/95 px-3 py-1.5 shadow-[0_3px_0_rgb(var(--w-ink))]">
            {naming !== null ? (
              <form
                className="min-w-0"
                onSubmit={(e) => {
                  e.preventDefault();
                  const title = naming.trim();
                  setNaming(null);
                  if (title && title !== report.title) onRename(title).catch(() => setError("The new name did not save. Try again."));
                }}
              >
                <input
                  autoFocus
                  value={naming}
                  maxLength={200}
                  aria-label="Workspace name"
                  onChange={(e) => setNaming(e.target.value)}
                  onBlur={(e) => e.currentTarget.form?.requestSubmit()}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      e.preventDefault();
                      e.stopPropagation();
                      setNaming(null);
                    }
                  }}
                  className="h-7 w-[min(18rem,50vw)] rounded-md border-2 border-w-ink bg-w-field px-1.5 text-w-base font-bold text-w-strong outline-none focus:ring-2 focus:ring-w-teal"
                />
              </form>
            ) : canEdit ? (
              <button type="button" disabled={movingFolder} onClick={() => { setFoldering(null); setNaming(report.title); }} title="Rename" className={cn("truncate rounded-md text-left text-w-base font-bold text-w-strong hover:bg-w-sand", FOCUS)}>
                {report.title}
              </button>
            ) : (
              <span className="truncate text-w-base font-bold text-w-strong">{report.title}</span>
            )}
            {canEdit && (
              <button
                ref={folderButton}
                type="button"
                disabled={movingFolder}
                aria-expanded={foldering !== null}
                aria-label={`Move workspace to folder${report.folder ? `, currently ${report.folder}` : ", currently top level"}`}
                title="Move workspace to folder"
                onClick={() => {
                  setNaming(null);
                  setFolderError(null);
                  setFoldering((current) => current === null ? report.folder : null);
                }}
                className={cn("flex h-7 min-w-0 items-center gap-1 rounded-md px-1.5 text-w-xs text-w-muted hover:bg-w-sand hover:text-w-text", FOCUS)}
              >
                <FolderSimple size={14} weight={foldering !== null ? "fill" : "regular"} className="shrink-0" />
                <span className="hidden max-w-32 truncate @xl/board:inline">{report.folder || "Top level"}</span>
              </button>
            )}
            <span className="hidden shrink-0 text-w-xs text-w-muted @xl/board:inline">
              {doc.items.length} {doc.items.length === 1 ? "item" : "items"} · {openMarks} open {openMarks === 1 ? "mark" : "marks"}
            </span>
            <AnimatePresence>
              {foldering !== null && (
                <motion.form
                  {...POPOVER_MOTION}
                  onSubmit={(e) => {
                    e.preventDefault();
                    void moveFolder();
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Escape" && !movingFolder) {
                      e.preventDefault();
                      e.stopPropagation();
                      closeFolder();
                    }
                  }}
                  className="absolute left-0 top-full z-30 mt-2 w-72 max-w-[calc(100vw-1.5rem)] space-y-2 rounded-lg border-2 border-w-ink bg-w-paper p-3 shadow-[0_3px_0_rgb(var(--w-ink))]"
                >
                  <label htmlFor={`workspace-folder-${report.id}`} className="block text-w-xs font-semibold text-w-text2">Move workspace to folder</label>
                  <input
                    id={`workspace-folder-${report.id}`}
                    list={`workspace-folders-${report.id}`}
                    autoFocus
                    value={foldering}
                    onChange={(e) => setFoldering(e.target.value)}
                    placeholder="Top level"
                    aria-describedby={`workspace-folder-help-${report.id}`}
                    className="h-9 w-full rounded-md border-2 border-w-ink bg-w-field px-2 text-w-sm text-w-text outline-none placeholder:text-w-faint focus:ring-2 focus:ring-w-teal"
                  />
                  <datalist id={`workspace-folders-${report.id}`}>
                    {folders.map((folder) => <option key={folder} value={folder} />)}
                  </datalist>
                  <p id={`workspace-folder-help-${report.id}`} className="text-w-2xs text-w-muted">Leave blank for top level.</p>
                  {folderError && <p role="alert" className="text-w-xs text-w-danger">{folderError}</p>}
                  <div className="flex justify-end gap-1.5">
                    <button type="button" disabled={movingFolder} onClick={closeFolder} className={cn(BTN_GHOST, BTN_SM)}>Cancel</button>
                    <button type="submit" disabled={movingFolder} className={cn(BTN_PRIMARY, BTN_SM)}>{movingFolder ? "Moving" : "Move"}</button>
                  </div>
                </motion.form>
              )}
            </AnimatePresence>
          </div>
          <div className="pointer-events-auto flex shrink-0 items-center gap-0.5 rounded-lg border-2 border-w-ink bg-w-paper/95 p-0.5 shadow-[0_3px_0_rgb(var(--w-ink))]">
            <ClusterButton icon={ChatCircleDots} label="Marks" active={marksOpen} onClick={() => setMarksOpen((v) => !v)} />
            <ClusterButton
              icon={TreeStructure}
              label="Links to and from this workspace"
              active={linksOpen}
              onClick={() => {
                setLinksOpen((v) => !v);
                setMarksOpen(false);
              }}
            />
            <ClusterButton icon={Quotes} label="Quote this workspace in chat" onClick={quoteCanvas} />
            {ownCanvas && <ClusterButton icon={Phone} label="Call your main agent about this workspace" onClick={callAbout} />}
            {onWorkspace && !inWorkspace && <ClusterButton icon={Layout} label="Open as workspace, with your chat and channels beside it" onClick={onWorkspace} />}
            {ownCanvas && <ClusterButton icon={GraduationCap} label={practice.busy ? "Making the lesson" : practice.error ?? PRACTICE_THIS_HINT} active={practice.busy} onClick={practice.run} />}
            <ClusterButton icon={ClockCounterClockwise} label="History" onClick={onHistory} />
            <ClusterButton icon={copied ? Check : LinkIcon} label={copied ? "Link copied" : "Copy a link"} onClick={() => copy(workspaceHref(window.location.origin, report.id))} />
          </div>
        </div>

        <AnimatePresence>
          {marksOpen && (
            <motion.aside {...POPOVER_MOTION} className="absolute right-3 top-16 z-20 flex max-h-[60%] w-80 flex-col rounded-lg border-2 border-w-ink bg-w-paper shadow-[0_3px_0_rgb(var(--w-ink))]">
              <div className="flex items-center justify-between border-b-2 border-w-line px-3 py-2">
                <h3 className="text-w-sm font-semibold text-w-strong">Marks</h3>
                <button type="button" onClick={() => setMarksOpen(false)} aria-label="Close marks" className="grid h-7 w-7 place-items-center rounded-md text-w-muted hover:bg-w-sand">
                  <X size={14} weight="bold" />
                </button>
              </div>
              <ul className="nowheel min-h-0 flex-1 space-y-1 overflow-y-auto p-2">
                {marks.length === 0 && <li className="px-2 py-3 text-w-sm text-w-muted">No marks yet. Pick the pin (M) or sticky (S) and click where you want a change.</li>}
                {[...marks].reverse().map((m) => (
                  <li key={m.id}>
                    <button
                      type="button"
                      onClick={() => {
                        focusItem(m.item_id);
                        setOpenMark(m.id);
                      }}
                      className={cn("w-full rounded-md px-2 py-1.5 text-left transition-colors hover:bg-w-sand", EASE)}
                    >
                      <span className={cn("line-clamp-2 text-w-sm", m.status === "open" ? "text-w-strong" : "text-w-muted line-through")}>{m.body}</span>
                      {m.reply && <span className="mt-0.5 line-clamp-2 block text-w-xs text-w-text2">{m.reply}</span>}
                    </button>
                  </li>
                ))}
              </ul>
            </motion.aside>
          )}
        </AnimatePresence>

        <AnimatePresence>
          {linksOpen && (
            <motion.aside {...POPOVER_MOTION} className="absolute right-3 top-16 z-20 flex max-h-[60%] w-80 max-w-[calc(100%-1.5rem)] flex-col rounded-lg border-2 border-w-ink bg-w-paper shadow-[0_3px_0_rgb(var(--w-ink))]">
              <div className="flex items-center justify-between border-b-2 border-w-line px-3 py-2">
                <h3 className="text-w-sm font-semibold text-w-strong">Links</h3>
                <button type="button" onClick={() => setLinksOpen(false)} aria-label="Close links" className="grid h-7 w-7 place-items-center rounded-md text-w-muted hover:bg-w-sand">
                  <X size={14} weight="bold" />
                </button>
              </div>
              <div className="nowheel min-h-0 flex-1 space-y-3 overflow-y-auto p-2">
                {links.map(({ label, list }) => (
                  <section key={label}>
                    <h4 className="px-2 pb-1 text-w-2xs font-semibold text-w-muted">{label}</h4>
                    {list.length === 0 ? (
                      <p className="px-2 text-w-xs text-w-faint">{label === "Links to" ? "Notes with [[Report title]] or report cards link out." : "No report links here yet."}</p>
                    ) : (
                      <ul>
                        {list.map((r) => (
                          <li key={r.id}>
                            <button type="button" onClick={() => openReport(r.id)} className={cn("w-full truncate rounded-md px-2 py-1.5 text-left text-w-sm text-w-text hover:bg-w-sand", EASE)}>
                              {r.title}
                              {r.kind === "canvas" && <span className="ml-1.5 text-w-2xs text-w-muted">workspace</span>}
                            </button>
                          </li>
                        ))}
                      </ul>
                    )}
                  </section>
                ))}
              </div>
            </motion.aside>
          )}
        </AnimatePresence>

        <AnimatePresence>
          {tool === "embed" && (
            <motion.div {...POPOVER_MOTION} className="absolute bottom-16 left-1/2 z-30 w-[24rem] max-w-[calc(100%-1.5rem)] -translate-x-1/2">
              <EmbedPicker meId={meId} onPick={(drop) => void addEmbeds([drop])} onClose={() => setTool("select")} />
            </motion.div>
          )}
        </AnimatePresence>

        <AnimatePresence>
          {tool === "thing" && (
            <motion.div {...POPOVER_MOTION} className="absolute bottom-16 left-1/2 z-30 w-[24rem] max-w-[calc(100%-1.5rem)] -translate-x-1/2">
              <ThingPicker onPick={(targets) => void addThings(targets)} onClose={() => setTool("select")} />
            </motion.div>
          )}
        </AnimatePresence>

        <AnimatePresence>
          {composer && (
            <motion.form
              {...POPOVER_MOTION}
              onSubmit={(e) => {
                e.preventDefault();
                void submitComposer();
              }}
              style={{ left: Math.min(composer.screen.x, (wrap.current?.clientWidth ?? 600) - 300), top: Math.min(composer.screen.y + 8, (wrap.current?.clientHeight ?? 400) - 170) }}
              className={cn("absolute z-30 w-72 space-y-2 rounded-lg border-2 border-w-ink p-2.5 shadow-[0_3px_0_rgb(var(--w-ink))]", composer.kind === "sticky" ? STICKY.rose : "bg-w-paper")}
            >
              <p className="text-w-xs font-semibold text-w-text2">
                {composer.kind === "sticky" ? "Sticky note" : composer.item ? `Mark on ${composer.item.caption || composer.item.kind}` : "Mark here"}
                {author && <span className="font-normal text-w-muted"> · {author.name} will pick it up</span>}
              </p>
              <textarea
                autoFocus
                value={draftText}
                onChange={(e) => setDraftText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                    e.preventDefault();
                    void submitComposer();
                  }
                  if (e.key === "Escape") {
                    e.preventDefault();
                    setComposer(null);
                  }
                }}
                rows={3}
                maxLength={2000}
                placeholder={composer.kind === "sticky" ? "What do you think?" : "What should change here?"}
                className="w-full resize-none rounded-md border-2 border-w-ink bg-w-field px-2 py-1.5 text-w-sm outline-none placeholder:text-w-faint focus:ring-2 focus:ring-w-teal"
              />
              <div className="flex items-center justify-end gap-1.5">
                <button type="button" onClick={() => setComposer(null)} className={cn(BTN_GHOST, "h-8 text-w-xs")}>
                  Cancel
                </button>
                <button type="submit" disabled={!draftText.trim() || busy} className={cn(BTN_PRIMARY, BTN_SM)}>
                  {composer.kind === "sticky" ? "Stick it" : "Pin it"}
                </button>
              </div>
            </motion.form>
          )}
        </AnimatePresence>

        {error && (
          <p role="alert" className="absolute left-1/2 top-16 z-30 -translate-x-1/2 rounded-lg border-2 border-w-danger/40 bg-w-rose px-3 py-1.5 text-w-sm text-w-danger">
            {error}
          </p>
        )}
        {doc.items.length === 0 && canEdit && tool === "select" && (
          <div className="pointer-events-none absolute inset-0 z-10 grid place-items-center p-6">
            <p className="max-w-sm text-balance text-center text-w-sm text-w-muted">
              An empty workspace. Add a picture (I), a sticky note (S), a quest, report, to-do or message as a card (L), a live terminal or running quest (E), or ask an agent to fill it.
            </p>
          </div>
        )}
        {note && (
          <p role="status" className="absolute left-1/2 top-16 z-30 -translate-x-1/2 rounded-lg bg-w-invert px-3 py-1 text-w-xs text-w-oninvert">
            {note}
          </p>
        )}
        {undoing && (
          <div role="status" className="absolute bottom-16 left-1/2 z-30 flex -translate-x-1/2 items-center gap-3 whitespace-nowrap rounded-lg bg-w-invert py-0.5 pl-3 pr-1 text-w-sm text-w-oninvert animate-[fade-in-up_180ms_ease-out_both] motion-reduce:animate-none">
            <span>{undoing.item ? "Sticky note deleted" : "Mark deleted"}</span>
            <button type="button" onClick={undoDelete} className={cn("h-11 rounded-md px-2 font-semibold text-w-honey hover:underline sm:h-8", FOCUS)}>
              Undo
            </button>
          </div>
        )}
        {arrowFrom && <p className="absolute left-1/2 top-16 z-20 -translate-x-1/2 rounded-lg bg-w-invert px-3 py-1 text-w-xs text-w-oninvert">Now click the item the arrow points to</p>}
        {chosen.length > 0 && !arrowFrom && (
          <SelectionBar count={chosen.length} agents={agents} actions={guide} channelHint={inWorkspace && workspaceChannel ? "in the open channel" : null} />
        )}

        <div role="toolbar" aria-label="Workspace tools" className="absolute bottom-3 left-1/2 z-20 flex -translate-x-1/2 items-center gap-0.5 rounded-lg border-2 border-w-ink bg-w-paper/95 p-0.5 shadow-[0_3px_0_rgb(var(--w-ink))]">
          {tools.map((t) => (
            <ClusterButton key={t.tool} icon={t.icon} label={t.label} hint={t.key} active={t.tool === tool} onClick={() => pick(t.tool)} />
          ))}
          <input ref={fileInput} type="file" accept="image/*,video/mp4,video/webm,video/quicktime,.glb,model/gltf-binary" multiple hidden onChange={(e) => void addFiles(e.target.files)} />
        </div>

        <div className="absolute bottom-16 right-3 z-20 flex items-center gap-0.5 @3xl/board:bottom-3 rounded-lg border-2 border-w-ink bg-w-paper/95 p-0.5 shadow-[0_3px_0_rgb(var(--w-ink))]">
          <ClusterButton icon={MapTrifold} label="Minimap" active={minimap} onClick={() => setMinimap((v) => !v)} />
          <ClusterButton icon={Minus} label="Zoom out" onClick={() => void flow.zoomOut({ duration: reduced() ? 0 : 180 })} />
          <span className="min-w-11 select-none text-center text-w-xs font-medium tabular-nums text-w-text2">{Math.round(zoom * 100)}%</span>
          <ClusterButton icon={Plus} label="Zoom in" onClick={() => void flow.zoomIn({ duration: reduced() ? 0 : 180 })} />
          <ClusterButton icon={CornersOut} label="Fit to view" onClick={fit} />
        </div>
      </div>
      </EmbedStage>
      <AnimatePresence>{viewingAt >= 0 && <Lightbox key="media" items={media} start={viewingAt} onClose={closeMedia} />}</AnimatePresence>
    </CanvasCtx.Provider>
  );
}

type Thing = { target: string; kind: string; title: string; detail?: string };

const REF = /^(quest|report|todo|msg|agent):([A-Za-z0-9_-]{3,80})$/i;

function thingsFrom(results: SearchResults): Thing[] {
  return [
    ...results.quests.map((q) => ({ target: `quest:${q.id}`, kind: "Quest", title: q.title, detail: q.status.replace(/_/g, " ") })),
    ...(results.reports ?? []).map((r) => ({ target: `report:${r.id}`, kind: r.kind === "canvas" ? "Workspace" : "Report", title: r.title, detail: r.folder || undefined })),
    ...results.todos.map((t) => ({ target: `todo:${t.id}`, kind: "To-do", title: t.text, detail: t.status })),
    ...results.messages.map((m) => ({ target: `msg:${m.ref}`, kind: "Message", title: m.excerpt, detail: m.kind === "channel" ? `${m.author} in #${m.channel_name ?? "a channel"}` : `${m.author} to ${m.agent_name ?? "an agent"}` })),
    ...results.agents.map((a) => ({ target: `agent:${a.slug}`, kind: "Agent", title: a.name, detail: a.expertise ?? undefined })),
  ];
}

function ThingPicker({ onPick, onClose }: { onPick: (targets: string[]) => void; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<Thing[] | null>(null);
  const [failed, setFailed] = useState(false);
  const typed = query.trim().match(REF);
  useEffect(() => {
    const q = query.trim();
    if (q.length < 2 || REF.test(q)) {
      setFound(null);
      return;
    }
    const stop = new AbortController();
    const timer = window.setTimeout(() => {
      worldApi
        .search(q, stop.signal)
        .then((r) => {
          setFailed(false);
          setFound(thingsFrom(r).slice(0, 12));
        })
        .catch((e) => {
          if (!stop.signal.aborted) setFailed(!!e);
        });
    }, 180);
    return () => {
      stop.abort();
      window.clearTimeout(timer);
    };
  }, [query]);
  const list: Thing[] = typed ? [{ target: `${typed[1].toLowerCase()}:${typed[2]}`, kind: "Card", title: query.trim() }] : (found ?? []);
  return (
    <div className="flex max-h-[min(26rem,60vh)] flex-col overflow-hidden rounded-lg border-2 border-w-ink bg-w-paper shadow-[0_3px_0_rgb(var(--w-ink))]">
      <div className="flex items-center gap-2 border-b-2 border-w-line p-2">
        <input
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              onClose();
            }
            if (e.key === "Enter" && list[0]) {
              e.preventDefault();
              onPick([list[0].target]);
            }
          }}
          aria-label="Find a quest, report, to-do, message or agent"
          placeholder="Find a quest, report, to-do or message, or paste quest:..."
          className="h-9 min-w-0 flex-1 rounded-md border-2 border-w-ink bg-w-field px-2 text-w-sm outline-none placeholder:text-w-faint focus:ring-2 focus:ring-w-teal"
        />
        <button type="button" onClick={onClose} aria-label="Close" className="grid h-8 w-8 shrink-0 place-items-center rounded-md text-w-muted hover:bg-w-sand">
          <X size={14} weight="bold" />
        </button>
      </div>
      <ul className="nowheel min-h-0 flex-1 overflow-y-auto p-1" aria-label="Things to place">
        {list.length === 0 && (
          <li className="px-2 py-3 text-w-sm text-w-muted">
            {failed ? "Search did not answer. Try again." : query.trim().length < 2 ? "Type to find something in Loka. It lands as a live card." : found ? "Nothing matches." : "Looking"}
          </li>
        )}
        {list.map((t) => (
          <li key={t.target}>
            <button type="button" onClick={() => onPick([t.target])} className={cn("flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left hover:bg-w-sand", EASE, FOCUS)}>
              <span className="mt-0.5 w-16 shrink-0 text-w-2xs font-semibold text-w-muted">{t.kind}</span>
              <span className="min-w-0 flex-1">
                <span className="line-clamp-1 text-w-sm text-w-text">{t.title}</span>
                {t.detail && <span className="line-clamp-1 text-w-xs text-w-muted">{t.detail}</span>}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

interface GuideActions {
  toChat: () => void;
  toChannel: (id: string) => void;
  askAgent: (agent: WorldAgent) => void;
}

function SelectionBar({ count, agents, actions, channelHint }: { count: number; agents: WorldAgent[]; actions: GuideActions; channelHint: string | null }) {
  const [open, setOpen] = useState<"channel" | "agent" | null>(null);
  const [channels, setChannels] = useState<ChannelSummary[] | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (open !== "channel" || channels) return;
    let live = true;
    worldApi
      .channels()
      .then((list) => live && setChannels(list.filter((c) => c.kind !== "dm" && c.member)))
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [open, channels]);
  const close = () => setOpen(null);
  const row = cn("flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-w-sm hover:bg-w-sand", EASE, FOCUS);
  return (
    <div
      role="toolbar"
      aria-label="Selected items"
      onKeyDown={(e) => {
        if (e.key === "Escape" && open) {
          e.stopPropagation();
          close();
        }
      }}
      className="absolute left-1/2 top-16 z-20 -translate-x-1/2"
    >
      <div className="flex items-center gap-1 rounded-lg border-2 border-w-ink bg-w-paper/95 p-0.5 pl-2.5 shadow-[0_3px_0_rgb(var(--w-ink))]">
        <span className="whitespace-nowrap text-w-xs font-semibold text-w-text2">{count} selected</span>
        <button type="button" onClick={actions.toChat} className={cn(BTN_GHOST, BTN_SM, "gap-1.5")}>
          <Quotes size={14} weight="bold" aria-hidden />
          {count > MAX_QUOTED ? `Quote the first ${MAX_QUOTED}` : "Quote in chat"}
        </button>
        <button type="button" aria-expanded={open === "channel"} onClick={() => setOpen(open === "channel" ? null : "channel")} className={cn(BTN_GHOST, BTN_SM, "gap-1.5")}>
          <Hash size={14} weight="bold" aria-hidden />
          Send to a channel
        </button>
        <button type="button" aria-expanded={open === "agent"} onClick={() => setOpen(open === "agent" ? null : "agent")} className={cn(BTN_GHOST, BTN_SM, "gap-1.5")}>
          <At size={14} weight="bold" aria-hidden />
          Ask an agent
        </button>
      </div>
      <AnimatePresence>
        {open && (
          <motion.div {...POPOVER_MOTION} className="nowheel absolute left-1/2 top-full mt-2 max-h-[min(20rem,50vh)] w-72 -translate-x-1/2 overflow-y-auto rounded-lg border-2 border-w-ink bg-w-paper p-1 shadow-[0_3px_0_rgb(var(--w-ink))]">
            {open === "channel" ? (
              channels === null ? (
                <p className="px-2 py-2 text-w-sm text-w-muted">{failed ? "Channels did not load. Try again." : "Loading channels"}</p>
              ) : channels.length === 0 ? (
                <p className="px-2 py-2 text-w-sm text-w-muted">You are not in any channel yet.</p>
              ) : (
                channels.map((c) => (
                  <button key={c.id} type="button" className={row} onClick={() => { actions.toChannel(c.id); close(); }}>
                    {c.kind === "team" ? <UsersThree size={14} weight="duotone" className="shrink-0 text-w-link" /> : <Hash size={14} weight="duotone" className="shrink-0 text-w-link" />}
                    <span className="min-w-0 flex-1 truncate">{c.name}</span>
                  </button>
                ))
              )
            ) : (
              <>
                <p className="px-2 pb-1 pt-1.5 text-w-xs text-w-muted">{channelHint ? `Mentions the agent ${channelHint}, with the items quoted` : "Opens its chat with the items quoted"}</p>
                {agents.map((a) => (
                  <button key={a.id} type="button" className={row} onClick={() => { actions.askAgent(a); close(); }}>
                    <AgentFace agent={a} size={20} rounded="rounded-md" />
                    <span className="min-w-0 flex-1 truncate">{a.name}</span>
                    <span className="shrink-0 text-w-xs text-w-muted">@{a.slug}</span>
                  </button>
                ))}
              </>
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function ClusterButton({ icon: Icon, label, hint, active, onClick }: { icon: PhosphorIcon; label: string; hint?: string; active?: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      title={hint ? `${label} (${hint})` : label}
      className={cn("grid h-8 w-8 place-items-center rounded-md transition-colors", EASE, FOCUS, active ? "bg-w-invert text-w-oninvert" : "text-w-text2 hover:bg-w-sand hover:text-w-strong")}
    >
      <Icon size={16} weight={active ? "fill" : "bold"} />
    </button>
  );
}

export default function ReportCanvas(props: BoardProps) {
  return (
    <ReactFlowProvider>
      <Board {...props} />
    </ReactFlowProvider>
  );
}
