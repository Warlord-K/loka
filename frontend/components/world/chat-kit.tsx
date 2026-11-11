"use client";

import { useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import ReactMarkdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { ArrowCounterClockwise, Brain, Check, CheckCircle, Copy, File as FileIcon, GitPullRequest, Lightning, Paperclip, PencilSimple, Play, Scroll, Terminal, Trash, WarningCircle, X } from "@phosphor-icons/react";
import { cn, safeDecode } from "@/lib/utils";
import { useCopy } from "@/lib/copy";
import { SCROLL_JUMP } from "@/lib/scroll";
import { codeLocalPaths, LocalPathContext, parseLocalPath, PathLink } from "./local-path";
import { causeLabel, dayLabel, draftDecision, messageTime, sameDay, worldApi, WorldApiError, type Attachment, type Draft, type BackendModels, type MessageMeta, type Person, type TurnOptions, type WorldAgent } from "@/lib/world";
import { BTN_PRIMARY, BTN_PRIMARY_SM, BTN_SECONDARY_SM, CHUNKY, FOCUS } from "./styles";
import { WCombobox, WSelect } from "./controls";
import { OpenWorkshop } from "./workbench/open-workshop";
import { textOf } from "./message-links";
import { LinkedText, ReviewDiff } from "./review-draft";
import { allowedEmbedUrl, embedFromHref, EmbedView, linkEmbeds, useChannelNames } from "./embeds";
import { headingSlug } from "@/lib/reports";
import { isMedia, MediaGrid, mediaUrl, ZoomImage } from "./media-view";
import { cliName, modelName } from "@/lib/model-names";
import { RunCommandContext } from "./run-command";

function handleOf(name: string): string {
  return name.replace(/[^A-Za-z0-9]/g, "").toLowerCase();
}

export function useAgentHandles(agents: WorldAgent[]): Map<string, WorldAgent> {
  return useMemo(() => {
    const seen = new Map<string, WorldAgent | null>();
    for (const agent of agents) {
      for (const value of [agent.slug, agent.name]) {
        const key = handleOf(value);
        if (!key) continue;
        const existing = seen.get(key);
        seen.set(key, !seen.has(key) || existing?.id === agent.id ? agent : null);
      }
    }
    const out = new Map<string, WorldAgent>();
    for (const [k, v] of seen) if (v) out.set(k, v);
    return out;
  }, [agents]);
}

export function usePersonHandles(people: Person[]): Map<string, Person> {
  return useMemo(() => {
    const seen = new Map<string, Person | null>();
    for (const person of people) {
      for (const value of [person.name ?? "", person.email?.split("@")[0] ?? ""]) {
        const key = handleOf(value);
        if (!key) continue;
        const existing = seen.get(key);
        seen.set(key, !seen.has(key) || existing?.id === person.id ? person : null);
      }
    }
    const out = new Map<string, Person>();
    for (const [k, v] of seen) if (v) out.set(k, v);
    return out;
  }, [people]);
}

function linkMentions(text: string, handles: Map<string, WorldAgent>, channels: Map<string, string>, people?: Map<string, Person>, prose = false, paths = false): string {
  return text
    .split(/(```[\s\S]*?```|`[^`\n]*`)/g)
    .map((part, i) =>
      i % 2
        ? part
        : linkEmbeds(paths ? codeLocalPaths(part) : part, channels)
            .replace(/(^|[\s(])@([A-Za-z0-9_.-]+)/g, (whole, lead: string, name: string) => {
              const key = handleOf(name);
              if (key === "here" || key === "channel") return `${lead}[@${key}](#everyone)`;
              const agent = handles.get(key);
              if (agent) return `${lead}[@${name}](#agent-${agent.id})`;
              const person = people?.get(handleOf(name));
              const label = (person?.name || person?.email?.split("@")[0] || name).replace(/[\\[\]]/g, "");
              return person ? `${lead}[@${label}](#person-${person.id})` : whole;
            })
            .replace(/([^\n])\n(?!\n|\s*([-*+] |\d+\. |#|>|\|))/g, prose ? "$1\n" : "$1  \n"),
    )
    .join("");
}

type HastNode = { type: string; value?: string; tagName?: string; properties?: Record<string, unknown>; children?: HastNode[] };

function hastText(node: HastNode): string {
  return node.type === "text" ? (node.value ?? "") : (node.children ?? []).map(hastText).join("");
}

function uiSource(node: unknown): string | null {
  const code = (node as HastNode | undefined)?.children?.find((c) => c.type === "element");
  const cls = code?.tagName === "code" ? code.properties?.className : undefined;
  return Array.isArray(cls) && cls.includes("language-ui") && code ? hastText(code) : null;
}

const UI_FENCE = /^ {0,3}(?:`{3,}|~{3,})[ \t]*ui(?:[ \t].*)?$/gm;

function uiIndex(markdown: string, node: unknown): number {
  const offset = (node as { position?: { start?: { offset?: number } } } | undefined)?.position?.start?.offset;
  return offset === undefined ? 0 : [...markdown.slice(0, offset).matchAll(UI_FENCE)].length;
}

const SHELL_LANGS = new Set(["bash", "sh", "shell", "zsh", "fish", "console", "shellsession"]);
const RUN_MAX = 2000;

export { RunCommandContext, type RunCommand } from "./run-command";

export function runnableCommand(node: unknown): string | null {
  const code = (node as HastNode | undefined)?.children?.find((c) => c.type === "element");
  const cls = code?.tagName === "code" ? code.properties?.className : undefined;
  if (!code || !Array.isArray(cls)) return null;
  const lang = cls.map(String).find((c) => c.startsWith("language-"))?.slice(9).toLowerCase();
  if (!lang || !SHELL_LANGS.has(lang)) return null;
  const command = hastText(code)
    .split("\n")
    .map((line) => line.replace(/^\s*[$%]\s+/, "").trimEnd())
    .join("\n")
    .trim();
  if (!command || command.length > RUN_MAX || /<[A-Za-z][\w -]*>|\bYOUR_[A-Z_]+\b/.test(command)) return null;
  return command;
}

const PR = /github\.com\/([^/\s]+\/[^/\s]+)\/pull\/(\d+)/;

export function PrChip({ url }: { url: string }) {
  const m = url.match(PR);
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      className="inline-flex items-center gap-1 rounded-md border-2 border-w-ink bg-w-aqua px-1.5 py-0.5 align-middle text-w-xs font-semibold text-w-link no-underline shadow-[0_2px_0_rgb(var(--w-ink))] hover:bg-w-field"
    >
      <GitPullRequest size={13} weight="bold" />
      {m ? `${m[1]}#${m[2]}` : "Pull request"}
    </a>
  );
}

export function RichText({
  text,
  handles,
  people,
  onOpenAgent,
  onOpenPerson,
  onWiki,
  renderUi,
  prose = false,
  className,
}: {
  text: string;
  handles: Map<string, WorldAgent>;
  people?: Map<string, Person>;
  onOpenAgent: (id: string) => void;
  onOpenPerson?: (id: string) => void;
  onWiki?: (title: string) => void;
  renderUi?: (source: string, index: number) => ReactNode;
  prose?: boolean;
  className?: string;
}) {
  const channels = useChannelNames();
  const openAgentRef = useRef(onOpenAgent);
  openAgentRef.current = onOpenAgent;
  const openPersonRef = useRef(onOpenPerson);
  openPersonRef.current = onOpenPerson;
  const wikiRef = useRef(onWiki);
  wikiRef.current = onWiki;
  const uiRef = useRef(renderUi);
  uiRef.current = renderUi;
  const pathAgent = useContext(LocalPathContext);
  const pathRef = useRef(pathAgent);
  pathRef.current = pathAgent;
  const markdown = linkMentions(text, handles, channels, people, prose, !!pathAgent);
  const markdownRef = useRef(markdown);
  markdownRef.current = markdown;
  const components: Components = useMemo(
    () => ({
      p: ({ children }) => <p className="my-1.5 first:mt-0 last:mb-0">{children}</p>,
      a: ({ href, title, children }) => {
        const embed = embedFromHref(href);
        if (embed) {
          const label = textOf(children).trim();
          const bare = !label || label === href || /^(msg|todo|quest|report):/i.test(label) || /^https?:\/\//i.test(label);
          return <EmbedView embed={embed} label={bare ? undefined : label} card={title === "card"} />;
        }
        const pathOwner = pathRef.current;
        const pathTarget = pathOwner && href ? parseLocalPath(safeDecode(href)) : null;
        if (pathOwner && pathTarget) {
          const label = textOf(children).trim();
          return (
            <PathLink agent={pathOwner} target={pathTarget}>
              {label && label !== href ? children : undefined}
            </PathLink>
          );
        }
        if (href?.startsWith("#agent-")) {
          const id = href.slice(7);
          return (
            <button type="button" onClick={() => openAgentRef.current(id)} className="rounded-md bg-w-teal/20 px-0.5 font-medium text-w-link hover:bg-w-teal/35">
              {children}
            </button>
          );
        }
        if (href === "#everyone") {
          return <span className="rounded-md bg-w-gold/25 px-0.5 font-semibold text-w-warn">{children}</span>;
        }
        if (href?.startsWith("#wiki-")) {
          const missing = href.startsWith("#wiki-~");
          const target = safeDecode(href.slice(missing ? 7 : 6));
          return (
            <button
              type="button"
              onClick={() => wikiRef.current?.(target)}
              title={missing ? `No report called "${target}" yet` : undefined}
              className={cn(
                "rounded-md px-0.5 font-medium underline decoration-dotted underline-offset-2",
                missing ? "text-w-muted decoration-w-line2 hover:bg-w-sand" : "bg-w-gold/20 text-w-link decoration-w-gold/60 hover:bg-w-gold/35",
              )}
            >
              {children}
            </button>
          );
        }
        if (href?.startsWith("#person-")) {
          const id = href.slice(8);
          return (
            <button type="button" onClick={() => openPersonRef.current?.(id)} className="rounded-md bg-w-teal/20 px-0.5 font-medium text-w-link hover:bg-w-teal/35">
              {children}
            </button>
          );
        }
        if (href && PR.test(href)) return <PrChip url={href} />;
        return (
          <a href={href} target="_blank" rel="noopener noreferrer" className="break-words text-w-link underline decoration-w-teal/50 hover:decoration-w-link">
            {children}
          </a>
        );
      },
      ul: ({ children }) => <ul className="my-1.5 ml-5 list-disc space-y-0.5">{children}</ul>,
      ol: ({ children }) => <ol className="my-1.5 ml-5 list-decimal space-y-0.5">{children}</ol>,
      h1: ({ children }) =>
        prose ? (
          <h2 id={headingSlug(textOf(children))} className="mb-2 mt-9 scroll-mt-4 text-[1.6rem] font-bold leading-tight tracking-[-0.01em] text-w-strong first:mt-0">{children}</h2>
        ) : (
          <p className="mb-1 mt-2 text-w-lg font-bold">{children}</p>
        ),
      h2: ({ children }) =>
        prose ? (
          <h3 id={headingSlug(textOf(children))} className="mb-1.5 mt-8 scroll-mt-4 text-[1.3rem] font-semibold leading-snug tracking-[-0.01em] text-w-strong first:mt-0">{children}</h3>
        ) : (
          <p className="mb-1 mt-2 text-w-md font-bold">{children}</p>
        ),
      h3: ({ children }) =>
        prose ? (
          <h4 id={headingSlug(textOf(children))} className="mb-1 mt-6 scroll-mt-4 text-w-lg font-semibold text-w-strong first:mt-0">{children}</h4>
        ) : (
          <p className="mb-1 mt-2 font-bold">{children}</p>
        ),
      h4: ({ children }) =>
        prose ? (
          <h5 id={headingSlug(textOf(children))} className="mb-1 mt-5 scroll-mt-4 font-semibold text-w-text2">{children}</h5>
        ) : (
          <p className="mb-1 mt-2 font-bold">{children}</p>
        ),
      blockquote: ({ children }) => <blockquote className="my-1.5 border-l-4 border-w-line2 pl-3 text-w-text2">{children}</blockquote>,
      code: ({ className: cls, children }) => {
        if (cls) return <code className={cls}>{children}</code>;
        const agent = pathRef.current;
        const target = agent ? parseLocalPath(textOf(children)) : null;
        if (agent && target) return <PathLink agent={agent} target={target} />;
        return <code className="rounded-md bg-w-sand px-1 py-px font-mono text-[0.82em] text-w-warn">{children}</code>;
      },
      pre: ({ children, node }) => {
        const source = uiRef.current ? uiSource(node) : null;
        return source !== null && uiRef.current ? uiRef.current(source, uiIndex(markdownRef.current, node)) : <CodeBlock command={runnableCommand(node)}>{children}</CodeBlock>;
      },
      table: ({ children }) => (
        <div className={cn("overflow-x-auto rounded-lg border-2 border-w-line", prose ? "my-5" : "my-2")}>
          <table className={cn("min-w-full border-collapse [&_tr:last-child>td]:border-b-0 [&_tr>*:last-child]:border-r-0", prose ? "text-w-base" : "text-w-sm")}>{children}</table>
        </div>
      ),
      th: ({ children }) => <th className="border-b-2 border-r border-w-line bg-w-sand/60 px-3 py-2 text-left text-w-sm font-semibold text-w-text2">{children}</th>,
      td: ({ children }) => <td className="border-b border-r border-w-line px-3 py-2 align-top">{children}</td>,
      img: ({ src, alt }) =>
        typeof src !== "string" ? null : localImage(src) ? (
          <span className="my-1 inline-block">
            <ZoomImage src={src} alt={alt ?? ""} className="max-h-72 rounded-lg border-2 border-w-ink" />
          </span>
        ) : (
          <a href={src} target="_blank" rel="noopener noreferrer" className="underline">
            {alt || "Image"} ({hostOf(src)})
          </a>
        ),
      hr: () => <hr className="my-3 border-dashed border-w-line2" />,
    }),
    [prose],
  );
  return (
    <div className={cn("break-words [overflow-wrap:anywhere]", className)}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={components}
        urlTransform={(url) => (url === "#everyone" || url.startsWith("#agent-") || url.startsWith("#person-") || url.startsWith("#wiki-") || allowedEmbedUrl(url) ? url : defaultUrlTransform(url))}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}

function localImage(src: string) {
  return src.startsWith("/") && !src.startsWith("//") && !src.includes("\\");
}

function hostOf(src: string) {
  try {
    return new URL(src).host || src;
  } catch {
    return src;
  }
}

type RunState = "idle" | "armed" | "sending" | "sent";

function CodeBlock({ children, command = null }: { children: React.ReactNode; command?: string | null }) {
  const ref = useRef<HTMLPreElement>(null);
  const runner = useContext(RunCommandContext);
  const [state, setState] = useState<RunState>("idle");
  const [problem, setProblem] = useState<string | null>(null);
  const runnable = !!runner && !!command;
  useEffect(() => {
    if (state !== "sent") return;
    const id = window.setTimeout(() => setState("idle"), 2400);
    return () => window.clearTimeout(id);
  }, [state]);
  const run = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!runner || !command || state === "sending") return;
    setState("sending");
    setProblem(null);
    try {
      await runner.run(command);
      setState("sent");
    } catch (err) {
      setProblem(err instanceof Error && err.message ? err.message : "That did not start. Try again.");
      setState("idle");
    }
  };
  return (
    <div className="group/code relative my-2">
      <pre ref={ref} className={cn("overflow-x-auto rounded-lg border-2 border-w-line2 bg-w-sand/60 p-3 font-mono text-w-xs leading-relaxed text-w-strong", runnable ? "pr-[4.75rem]" : "pr-10")}>{children}</pre>
      <div className="absolute right-1.5 top-1.5 flex items-center gap-1">
        {runnable && state !== "armed" && (
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setProblem(null);
              setState("armed");
            }}
            disabled={state === "sending"}
            aria-label={state === "sent" ? "Started" : "Run this command"}
            title={state === "sent" ? "Started" : `Run ${runner.where}`}
            className={cn(
              "grid h-7 w-7 place-items-center rounded-md border-2 border-w-ink bg-w-teal text-w-strong transition-opacity hover:bg-w-teal2 focus-visible:opacity-100 group-hover/code:opacity-100 disabled:opacity-60 [@media(hover:none)]:opacity-100",
              state === "idle" ? "opacity-0" : "opacity-100",
              state === "sent" && "bg-w-card text-w-ok",
            )}
          >
            {state === "sent" ? <Check size={14} weight="bold" /> : <Play size={13} weight="fill" />}
          </button>
        )}
        <CornerCopy text={() => ref.current?.innerText ?? ""} label="Copy code" />
      </div>
      {runnable && state === "armed" && (
        <div role="group" aria-label="Run this command" className="mt-1 flex flex-wrap items-center gap-2 rounded-lg border-2 border-w-line2 bg-w-card px-2 py-1.5 text-w-xs">
          <Terminal size={14} weight="bold" className="shrink-0 text-w-muted" />
          <span className="min-w-0 flex-1 text-w-text2">Run this {runner.where}? The output shows here and goes back to {runner.who}.</span>
          <button type="button" autoFocus onClick={(e) => void run(e)} className={BTN_PRIMARY_SM}>
            Run
          </button>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setState("idle");
            }}
            className={BTN_SECONDARY_SM}
          >
            Cancel
          </button>
        </div>
      )}
      {problem && <p role="alert" className="mt-1 text-w-xs text-w-danger">{problem}</p>}
    </div>
  );
}
function scrollBox(el: HTMLElement | null): HTMLElement | null {
  for (let node = el?.parentElement; node; node = node.parentElement) {
    if (/(auto|scroll)/.test(getComputedStyle(node).overflowY)) return node;
  }
  return null;
}

const TEXT_ENTRY = "textarea, input:not([type]), input[type=text], input[type=search], input[type=url], input[type=email], input[type=number], input[type=tel], input[type=password]";

function editingIn(box: HTMLElement): boolean {
  const active = document.activeElement;
  return active instanceof HTMLElement && box.contains(active) && (active.matches(TEXT_ENTRY) || active.isContentEditable);
}

export function useRefetchOnResume(refetch: () => void) {
  const latest = useRef(refetch);
  latest.current = refetch;
  useEffect(() => {
    let timer: number | undefined;
    const schedule = () => {
      if (document.visibilityState !== "visible") return;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => latest.current(), 300);
    };
    window.addEventListener("world-reconnected", schedule);
    window.addEventListener("focus", schedule);
    document.addEventListener("visibilitychange", schedule);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("world-reconnected", schedule);
      window.removeEventListener("focus", schedule);
      document.removeEventListener("visibilitychange", schedule);
    };
  }, []);
}

const STICK_GAP = 48;
const INTENT_MS = 1000;
const PIN_GAP = 8;
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);

function gapOf(box: HTMLElement): number {
  return box.scrollHeight - box.clientHeight - box.scrollTop;
}

function firstVisible(box: HTMLElement): { el: Element; top: number } | null {
  const top = box.getBoundingClientRect().top;
  let found: { el: Element; top: number } | null = null;
  let scope: Element = box;
  for (let depth = 0; depth < 4; depth++) {
    const next = Array.from(scope.children).find((el) => {
      const rect = el.getBoundingClientRect();
      return rect.height > 0 && rect.bottom > top;
    });
    if (!next) break;
    const rect = next.getBoundingClientRect();
    found = { el: next, top: rect.top - top };
    if (rect.top >= top || next.children.length === 0) break;
    scope = next;
  }
  return found;
}

export function useFollowBottom(endRef: RefObject<HTMLElement | null>, last: unknown, reset?: unknown) {
  const pinned = useRef(true);
  const anchor = useRef<{ el: Element; top: number } | null>(null);
  const shownReset = useRef<unknown>(Symbol());
  const [unseen, setUnseen] = useState(false);
  const [away, setAway] = useState(false);

  const toBottom = useCallback((box: HTMLElement) => {
    box.scrollTop = box.scrollHeight;
    anchor.current = null;
  }, []);

  const attached = useRef<{ box: HTMLElement; detach: () => void } | null>(null);
  useEffect(() => () => attached.current?.detach(), []);
  useLayoutEffect(() => {
    const box = scrollBox(endRef.current);
    if (attached.current?.box === box) return;
    attached.current?.detach();
    attached.current = null;
    if (!box) return;
    box.style.overflowAnchor = "none";
    pinned.current = true;
    setAway(false);
    setUnseen(false);
    toBottom(box);
    let lastTop = box.scrollTop;
    let dragging = false;
    let touchY: number | null = null;
    let intentAt = 0;
    const intent = () => {
      intentAt = performance.now();
    };
    const release = () => {
      intent();
      if (box.scrollTop <= 0) return;
      pinned.current = false;
      anchor.current = firstVisible(box);
    };
    const onWheel = (e: WheelEvent) => {
      intent();
      if (e.deltaY < 0 && !e.ctrlKey) release();
    };
    const onTouchStart = (e: TouchEvent) => {
      intent();
      touchY = e.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (e: TouchEvent) => {
      intent();
      const y = e.touches[0]?.clientY;
      if (touchY !== null && y !== undefined && y > touchY + 2) release();
      if (y !== undefined) touchY = y;
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLElement && (e.target.matches(TEXT_ENTRY) || e.target.isContentEditable)) return;
      intent();
      if (UP_KEYS.has(e.key) || (e.key === " " && e.shiftKey)) release();
    };
    const onPointerDown = (e: PointerEvent) => {
      intent();
      if (e.target === box) dragging = true;
    };
    const onPointerUp = () => {
      dragging = false;
    };
    const onScroll = () => {
      const gap = gapOf(box);
      const moved = box.scrollTop - lastTop;
      lastTop = box.scrollTop;
      const meant = dragging || performance.now() - intentAt < INTENT_MS;
      if (gap <= PIN_GAP || (moved > 0 && gap <= STICK_GAP && !dragging)) pinned.current = true;
      else if (moved < -1 && (dragging || (meant && gap > STICK_GAP))) pinned.current = false;
      else if (pinned.current && !meant && !editingIn(box)) {
        toBottom(box);
        lastTop = box.scrollTop;
        return;
      }
      anchor.current = pinned.current ? null : firstVisible(box);
      setAway(!pinned.current && gap > box.clientHeight * 1.5);
      if (pinned.current) setUnseen(false);
    };
    const settle = () => {
      if (pinned.current) {
        if (!editingIn(box)) toBottom(box);
      } else if (anchor.current?.el.isConnected) {
        const top = box.getBoundingClientRect().top;
        const now = anchor.current.el.getBoundingClientRect().top - top;
        const shift = now - anchor.current.top;
        if (Math.abs(shift) >= 1) box.scrollTop += shift;
        anchor.current = firstVisible(box);
      } else {
        anchor.current = firstVisible(box);
      }
      lastTop = box.scrollTop;
    };
    const resized = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(settle);
    const watch = (node: Node) => {
      if (node instanceof Element) resized?.observe(node);
    };
    resized?.observe(box);
    Array.from(box.children).forEach(watch);
    const mutated = new MutationObserver((records) => {
      for (const r of records) r.addedNodes.forEach(watch);
      settle();
    });
    mutated.observe(box, { childList: true });
    box.addEventListener("scroll", onScroll, { passive: true });
    box.addEventListener("wheel", onWheel, { passive: true });
    box.addEventListener("touchstart", onTouchStart, { passive: true });
    box.addEventListener("touchmove", onTouchMove, { passive: true });
    box.addEventListener("keydown", onKey);
    box.addEventListener("pointerdown", onPointerDown);
    box.addEventListener(SCROLL_JUMP, intent);
    box.addEventListener("animationend", settle);
    box.addEventListener("transitionend", settle);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    attached.current = {
      box,
      detach: () => {
        box.removeEventListener("scroll", onScroll);
        box.removeEventListener("wheel", onWheel);
        box.removeEventListener("touchstart", onTouchStart);
        box.removeEventListener("touchmove", onTouchMove);
        box.removeEventListener("keydown", onKey);
        box.removeEventListener("pointerdown", onPointerDown);
        box.removeEventListener(SCROLL_JUMP, intent);
        box.removeEventListener("animationend", settle);
        box.removeEventListener("transitionend", settle);
        window.removeEventListener("pointerup", onPointerUp);
        window.removeEventListener("pointercancel", onPointerUp);
        mutated.disconnect();
        resized?.disconnect();
      },
    };
  });

  useLayoutEffect(() => {
    const box = scrollBox(endRef.current);
    if (!box) return;
    if (shownReset.current !== reset) {
      shownReset.current = reset;
      pinned.current = true;
      setAway(false);
      setUnseen(false);
      toBottom(box);
      return;
    }
    if (pinned.current) {
      if (!editingIn(box)) toBottom(box);
      setUnseen(false);
    } else if (last !== undefined) {
      setUnseen(true);
    }
  }, [endRef, last, reset, toBottom]);

  const jump = () => {
    const box = scrollBox(endRef.current);
    pinned.current = true;
    setUnseen(false);
    setAway(false);
    if (!box) return;
    const calm = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    box.scrollTo({ top: box.scrollHeight, behavior: calm ? "auto" : "smooth" });
  };
  const stick = () => {
    const box = scrollBox(endRef.current);
    pinned.current = true;
    setUnseen(false);
    setAway(false);
    if (!box) return;
    toBottom(box);
    requestAnimationFrame(() => {
      if (pinned.current) toBottom(box);
    });
  };
  return { unseen, away, jump, stick };
}

export function NewMessagesPill({ show, away = false, onClick }: { show: boolean; away?: boolean; onClick: () => void }) {
  if (!show && !away) return null;
  return (
    <div className="pointer-events-none sticky bottom-2 z-10 h-0">
      <div className="absolute inset-x-0 bottom-0 flex justify-center">
      <button
        type="button"
        onClick={onClick}
        className={cn(BTN_PRIMARY_SM, "pointer-events-auto animate-[fade-in-up_180ms_ease-out_both] motion-reduce:animate-none")}
      >
        {show ? "New messages" : "Jump to latest"}
      </button>
      </div>
    </div>
  );
}

export function MessageStamp({ createdAt, editedAt, className }: { createdAt: string; editedAt?: string | null; className?: string }) {
  return (
    <time
      dateTime={createdAt}
      title={`${new Date(createdAt).toLocaleString()}${editedAt ? `, edited ${new Date(editedAt).toLocaleString()}` : ""}`}
      className={cn("whitespace-nowrap text-w-2xs text-w-muted", className)}
    >
      {messageTime(createdAt)}
      {editedAt && " (edited)"}
    </time>
  );
}

export function DayDivider({ at, previous }: { at: string; previous?: string }) {
  if (previous && sameDay(previous, at)) return null;
  return (
    <div role="separator" aria-label={dayLabel(at)} className="flex items-center gap-3 py-1 text-w-xs font-semibold text-w-muted">
      <span className="h-px flex-1 border-t-2 border-dashed border-w-line" />
      <span>{dayLabel(at)}</span>
      <span className="h-px flex-1 border-t-2 border-dashed border-w-line" />
    </div>
  );
}

export function CornerCopy({ text, label, className }: { text: string | (() => string); label: string; className?: string }) {
  const [copied, copy] = useCopy();
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        void copy(typeof text === "function" ? text() : text);
      }}
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
      className={cn(
        "grid h-7 w-7 place-items-center rounded-md border border-w-line2 bg-w-card text-w-muted transition-opacity hover:text-w-strong focus-visible:opacity-100 group-hover/code:opacity-100 [@media(hover:none)]:opacity-100",
        copied ? "text-w-ok opacity-100" : "opacity-0",
        className,
      )}
    >
      {copied ? <Check size={14} weight="bold" /> : <Copy size={14} weight="bold" />}
    </button>
  );
}

export function CopyButton({ text, label = "Copy message", className }: { text: string; label?: string; className?: string }) {
  const [copied, copyText] = useCopy();
  if (!text.trim()) return null;
  const copy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    await copyText(text);
  };
  return (
    <button
      type="button"
      onClick={(e) => void copy(e)}
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
      className={cn(
        "rounded-md p-0.5 text-w-muted transition-opacity hover:bg-w-sand hover:text-w-strong focus-visible:opacity-100 group-hover:opacity-100 [@media(hover:none)]:opacity-100",
        copied ? "opacity-100 text-w-ok" : "opacity-0",
        className,
      )}
    >
      {copied ? <Check size={12} weight="bold" /> : <Copy size={12} weight="bold" />}
    </button>
  );
}

export function EditButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Edit message"
      title="Edit message"
      className="rounded-md p-0.5 text-w-muted opacity-0 transition-opacity hover:bg-w-sand hover:text-w-strong focus-visible:opacity-100 group-hover:opacity-100"
    >
      <PencilSimple size={12} weight="bold" />
    </button>
  );
}

export function DeleteButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Delete message"
      title="Delete message"
      className="rounded-md p-0.5 text-w-muted opacity-0 transition-opacity hover:bg-w-rose hover:text-w-danger focus-visible:opacity-100 group-hover:opacity-100"
    >
      <Trash size={12} weight="bold" />
    </button>
  );
}

export function RewindButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Rewind to before this message"
      title="Rewind to before this message"
      className="rounded-md p-0.5 text-w-muted opacity-0 transition-opacity hover:bg-w-sand hover:text-w-strong focus-visible:opacity-100 group-hover:opacity-100 [@media(hover:none)]:opacity-100"
    >
      <ArrowCounterClockwise size={12} weight="bold" />
    </button>
  );
}

export function MessageEditor({
  initial,
  onSave,
  onCancel,
  rewind = false,
}: {
  initial: string;
  onSave: (text: string) => Promise<void>;
  onCancel: () => void;
  rewind?: boolean;
}) {
  const [text, setText] = useState(initial);
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);
  const save = async () => {
    const next = text.trim();
    if (!next || saving) return;
    if (!rewind && next === initial.trim()) return onCancel();
    setSaving(true);
    setFailed(null);
    try {
      await onSave(next);
    } catch (e) {
      setFailed(rewind && e instanceof Error && e.message ? e.message : "Could not save. Try again.");
      setSaving(false);
    }
  };
  return (
    <div className="space-y-1.5">
      <textarea
        ref={ref}
        value={text}
        rows={Math.min(8, Math.max(2, text.split("\n").length))}
        aria-label="Edit message"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") onCancel();
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            void save();
          }
        }}
        className="w-full resize-y rounded-md border-2 border-w-ink bg-w-field px-2 py-1 text-w-base leading-relaxed outline-none focus:ring-2 focus:ring-w-teal"
      />
      {rewind && !failed && (
        <p className="text-w-xs text-w-muted">Saving rewinds the chat to here: everything after this message is removed and the agent forgets it.</p>
      )}
      <div className="flex items-center justify-end gap-2 text-w-xs">
        {failed && <span role="alert" className="mr-auto text-w-danger">{failed}</span>}
        <button type="button" onClick={onCancel} className="rounded-md px-2 py-0.5 hover:bg-w-sand">
          Cancel
        </button>
        <button type="button" disabled={saving || !text.trim()} onClick={() => void save()} className={BTN_PRIMARY_SM}>
          {saving ? "Saving" : rewind ? "Save and rewind" : "Save"}
        </button>
      </div>
    </div>
  );
}

const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

export function isImageType(mime: string): boolean {
  return IMAGE_TYPES.includes(mime);
}

function FileChip({ name, href, failed, faded }: { name: string; href?: string; failed?: boolean; faded?: boolean }) {
  const body = (
    <>
      <FileIcon size={16} weight="bold" className="shrink-0" />
      <span className="max-w-[12rem] truncate">{name || "file"}</span>
    </>
  );
  const className = cn(
    "flex h-10 items-center gap-1.5 rounded-lg border-2 border-w-ink bg-w-field px-2.5 text-w-xs text-w-text",
    faded && "opacity-50",
    failed && "border-w-danger opacity-60",
    href && "hover:bg-w-sand",
  );
  return href ? (
    <a href={href} download={name || true} className={className} title={name}>
      {body}
    </a>
  ) : (
    <span className={className} title={name}>
      {body}
    </span>
  );
}

export function AttachmentGrid({ items, collapsed }: { items: Attachment[]; collapsed?: boolean }) {
  if (!items.length) return null;
  const media = items.filter((a) => isMedia(a.mime));
  const files = items.filter((a) => !isMedia(a.mime));
  return (
    <>
      <MediaGrid items={media} collapsed={collapsed} />
      {files.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-2">
          {files.map((a) => (
            <FileChip key={a.id} name={a.name} href={mediaUrl(a)} />
          ))}
        </div>
      )}
    </>
  );
}

export function AskChoices({ choices, answered, onAnswer }: { choices: string[]; answered: boolean; onAnswer: (choice: string) => void }) {
  if (!choices.length || answered) return null;
  return (
    <div className="mt-2 flex flex-wrap gap-2">
      {choices.map((c, i) => (
        <button
          key={c}
          type="button"
          onClick={() => onAnswer(c)}
          className={cn(CHUNKY, "px-3 py-1 text-w-sm", i === 0 ? "bg-w-teal hover:bg-w-teal2" : "bg-w-field hover:bg-w-sand")}
        >
          {c}
        </button>
      ))}
    </div>
  );
}

export function DraftQueue({ drafts, answered, onAnswer }: { drafts: Draft[]; answered: boolean; onAnswer: (text: string) => Promise<boolean> }) {
  const [picks, setPicks] = useState(() => drafts.map((d) => ({ approved: true, body: d.body })));
  const [editing, setEditing] = useState<number | null>(null);
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);
  const [failed, setFailed] = useState(false);
  if (!drafts.length) return null;
  const locked = answered || sent;
  const submit = async () => {
    setSending(true);
    setFailed(false);
    setEditing(null);
    const ok = await onAnswer(draftDecision(drafts, picks));
    setSending(false);
    if (ok) setSent(true);
    else setFailed(true);
  };
  const approvedCount = picks.filter((p) => p.approved).length;
  const set = (i: number, patch: Partial<{ approved: boolean; body: string }>) =>
    setPicks((all) => all.map((p, j) => (j === i ? { ...p, ...patch } : p)));
  return (
    <div className="mt-2 space-y-2">
      {drafts.map((d, i) => (
        <div key={i} className={cn("rounded-md border-2 border-w-ink bg-w-field px-2.5 py-2", !picks[i].approved && "opacity-55")}>
          <div className="mb-1 flex items-center gap-2 text-w-xs text-w-muted">
            <span className="min-w-0 flex-1 truncate">
              <span className="font-semibold">{d.path ? `Review comment ${i + 1}` : `Draft ${i + 1}`}</span>
              {d.to ? ` to ${d.to}` : ""}
              {d.subject ? ` · ${d.subject}` : ""}
            </span>
            {sent && <span className="font-semibold text-w-link">{picks[i].approved ? "Approved" : "Skipped"}</span>}
            <CopyButton text={picks[i].body} label={d.path ? "Copy comment" : "Copy draft"} />
            {!locked && (
              <>
                <button
                  type="button"
                  aria-label={editing === i ? `Done editing draft ${i + 1}` : `Edit draft ${i + 1}`}
                  onClick={() => setEditing(editing === i ? null : i)}
                  className={cn(CHUNKY, "bg-w-card p-1 text-w-strong hover:bg-w-sand")}
                >
                  {editing === i ? <Check size={12} weight="bold" /> : <PencilSimple size={12} weight="bold" />}
                </button>
                <button
                  type="button"
                  disabled={sending}
                  aria-pressed={!picks[i].approved}
                  onClick={() => set(i, { approved: !picks[i].approved })}
                  className={BTN_SECONDARY_SM}
                >
                  {picks[i].approved ? "Skip" : "Skipped, undo"}
                </button>
              </>
            )}
          </div>
          {d.path && <ReviewDiff draft={d} />}
          <div className={cn(d.path && "mt-1.5 rounded-md border-2 border-w-line bg-w-card px-2.5 py-2")}>
            {d.path && <p className="mb-1 text-w-xs font-semibold text-w-muted">Comment</p>}
            {editing === i && !locked ? (
              <textarea
                className="min-h-24 w-full resize-y rounded-md border border-w-line bg-w-field p-2 text-w-base leading-relaxed outline-none focus:border-w-teal"
                value={picks[i].body}
                onChange={(e) => set(i, { body: e.target.value })}
                aria-label={`Draft ${i + 1} text`}
              />
            ) : (
              <LinkedText text={picks[i].body} className="text-w-base leading-relaxed" />
            )}
          </div>
          {picks[i].body.trim() !== d.body.trim() && <p className="mt-1 text-w-2xs text-w-warn">Edited</p>}
        </div>
      ))}
      {!locked && (
        <button
          type="button"
          disabled={sending}
          onClick={() => void submit()}
          className={BTN_PRIMARY}
        >
          {sending
            ? "Sending..."
            : approvedCount === 0
              ? "Skip all"
              : drafts.length === 1
                ? "Send"
                : approvedCount === drafts.length
                  ? `Send all ${drafts.length}`
                  : `Send ${approvedCount} of ${drafts.length}`}
        </button>
      )}
      {sent && <p className="text-w-xs font-semibold text-w-link">Decisions sent.</p>}
      {failed && !locked && (
        <p role="alert" className="text-w-xs text-w-danger">
          That did not go through, so nothing was sent. Try again.
        </p>
      )}
    </div>
  );
}

export function QuestCard({
  meta,
  body,
  handles,
  onOpenAgent,
  onOpenSession,
  live,
}: {
  meta: MessageMeta;
  body: string;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  onOpenSession?: (sessionId: string) => void;
  live?: React.ReactNode;
}) {
  const failed = meta.status === "failed" || Boolean(meta.error);
  const started = meta.status === "started";
  const label = failed
    ? "needs attention"
    : started
      ? "started"
      : meta.merged
        ? "merged and done"
        : meta.status === "review"
          ? meta.watching ? "PR open, watching for reviews" : "ready for review"
          : "complete";
  return (
    <div
      className={cn(
        "max-w-[min(92%,46rem)] rounded-lg border-2 border-w-ink p-3 shadow-[0_3px_0_rgb(var(--w-ink))]",
        failed ? "bg-w-rose" : started ? "bg-w-card" : "bg-w-mint",
      )}
    >
      <p className="flex items-center gap-2 text-w-xs font-medium text-w-muted">
        {started ? <Lightning size={13} weight="fill" className="text-w-warn" /> : <Scroll size={13} weight="bold" />}
        Quest {label}
        {meta.verdict && (
          <span className="ml-auto inline-flex items-center gap-1 normal-case tracking-normal">
            {meta.verdict === "approve" ? <CheckCircle size={13} weight="fill" className="text-w-ok" /> : <WarningCircle size={13} weight="fill" className="text-w-alert" />}
            Reviewer: {meta.verdict}
          </span>
        )}
      </p>
      <p className="mt-1 font-semibold">{meta.title}</p>
      {started && !body && !live && <p className="mt-0.5 text-w-sm text-w-text2">Work started. Open the session to follow along.</p>}
      {live}
      {meta.watching && <p className="mt-0.5 text-w-sm text-w-text2">The PR is checked every 30 minutes. Review comments go back to the quest, and it finishes once the PR merges.</p>}
      {body && <RichText text={body} handles={handles} onOpenAgent={onOpenAgent} className="mt-1 text-w-base text-w-text" />}
      {meta.cause && (
        <p className="mt-1 text-w-sm text-w-danger">
          <span className="font-semibold">{causeLabel(meta.cause)}.</span> {meta.reason}
        </p>
      )}
      {meta.error && meta.error !== meta.reason && <p className="mt-1 text-w-sm text-w-danger">{meta.error}</p>}
      <div className="mt-2 flex flex-wrap gap-2">
        {meta.pr_url && <PrChip url={meta.pr_url} />}
        {meta.session_id && onOpenSession && (
          <button type="button" onClick={() => onOpenSession(meta.session_id as string)} className={BTN_SECONDARY_SM}>
            {started ? "Open the session" : "See the session"}
          </button>
        )}
        {meta.session_id && <OpenWorkshop detail={{ sessionId: meta.session_id as string, taskId: (meta.task_id as string | undefined) ?? null }} label="Open workshop" className="px-2 py-0.5 text-w-xs" />}
      </div>
    </div>
  );
}

export interface Pending {
  key: string;
  name: string;
  preview: string | null;
  attachment: Attachment | null;
  failed: boolean;
}

export function useAttachments() {
  const [pending, setPending] = useState<Pending[]>([]);
  const add = (files: Iterable<File>) => {
    for (const file of files) {
      const key = `${Date.now()}-${Math.random()}`;
      const preview = isImageType(file.type) ? URL.createObjectURL(file) : null;
      setPending((p) => [...p, { key, name: file.name, preview, attachment: null, failed: false }].slice(-8));
      worldApi
        .upload(file, file.name || (preview ? "pasted.png" : "file"))
        .then((attachment) => setPending((p) => p.map((x) => (x.key === key ? { ...x, attachment } : x))))
        .catch(() => setPending((p) => p.map((x) => (x.key === key ? { ...x, failed: true } : x))));
    }
  };
  const remove = (key: string) => setPending((p) => p.filter((x) => x.key !== key));
  const clear = () => setPending([]);
  const ready = pending.filter((p) => p.attachment).map((p) => (p.attachment as Attachment).id);
  const uploading = pending.some((p) => !p.attachment && !p.failed);
  const dropProps = {
    onPaste: (e: React.ClipboardEvent) => {
      const files = Array.from(e.clipboardData.files);
      if (files.length) {
        e.preventDefault();
        add(files);
      }
    },
    onDragOver: (e: React.DragEvent) => {
      if (Array.from(e.dataTransfer.items).some((i) => i.kind === "file")) e.preventDefault();
    },
    onDrop: (e: React.DragEvent) => {
      const files = Array.from(e.dataTransfer.files);
      if (files.length) {
        e.preventDefault();
        add(files);
      }
    },
  };
  return { pending, add, remove, clear, ready, uploading, dropProps };
}

export function AttachButton({ onFiles }: { onFiles: (files: File[]) => void }) {
  const ref = useRef<HTMLInputElement>(null);
  return (
    <>
      <input
        ref={ref}
        type="file"
        multiple
        className="hidden"
        onChange={(e) => {
          onFiles(Array.from(e.target.files ?? []));
          e.target.value = "";
        }}
      />
      <button
        type="button"
        onClick={() => ref.current?.click()}
        aria-label="Attach files"
        title="Attach images or files (or paste / drop them)"
        className={cn(CHUNKY, "grid h-10 w-10 shrink-0 place-items-center bg-w-card hover:bg-w-sand")}
      >
        <Paperclip size={17} weight="bold" />
      </button>
    </>
  );
}

export function PendingStrip({ pending, onRemove }: { pending: Pending[]; onRemove: (key: string) => void }) {
  if (!pending.length) return null;
  return (
    <div className="flex flex-wrap gap-2 px-3 pt-2 sm:px-5">
      {pending.map((p) => (
        <span key={p.key} className="relative">
          {p.preview ? (
            <img
              src={p.preview}
              alt=""
              className={cn("h-16 w-16 rounded-lg border-2 border-w-ink object-cover", !p.attachment && !p.failed && "opacity-50", p.failed && "border-w-danger opacity-60")}
            />
          ) : (
            <FileChip name={p.name} faded={!p.attachment && !p.failed} failed={p.failed} />
          )}
          <button
            type="button"
            aria-label="Remove attachment"
            onClick={() => onRemove(p.key)}
            className="absolute -right-1.5 -top-1.5 grid h-5 w-5 place-items-center rounded-full border-2 border-w-ink bg-w-field hover:bg-w-sand"
          >
            <X size={10} weight="bold" />
          </button>
        </span>
      ))}
    </div>
  );
}

const EFFORTS: Record<string, string[]> = {
  claude: ["low", "medium", "high", "xhigh", "max"],
  codex: ["minimal", "low", "medium", "high", "xhigh"],
};

export function useTurnOptions(agent: WorldAgent) {
  const key = `world.turn.${agent.id}`;
  const [options, setOptions] = useState<TurnOptions>({});
  useEffect(() => {
    try {
      const raw = window.localStorage.getItem(key);
      setOptions(raw ? (JSON.parse(raw) as TurnOptions) : {});
    } catch {
      setOptions({});
    }
  }, [key]);
  const update = (patch: TurnOptions) =>
    setOptions((cur) => {
      const next = { ...cur, ...patch };
      try {
        window.localStorage.setItem(key, JSON.stringify(next));
      } catch {}
      return next;
    });
  const effective: TurnOptions | undefined =
    options.model || options.effort || options.fast
      ? { model: options.model || undefined, effort: options.effort || undefined, fast: Boolean(options.fast) }
      : undefined;
  return { options, update, effective };
}

export function TurnControls({
  agent,
  options,
  onChange,
  onAgentUpdated,
}: {
  agent: WorldAgent;
  options: TurnOptions;
  onChange: (patch: TurnOptions) => void;
  onAgentUpdated?: (agent: WorldAgent) => void;
}) {
  const [backends, setBackends] = useState<Record<string, BackendModels>>({});
  const [switching, setSwitching] = useState(false);
  const [switchError, setSwitchError] = useState("");
  const targetCli = agent.pending_switch?.cli ?? agent.cli;
  const [ompModels, setOmpModels] = useState<string[]>([]);
  useEffect(() => {
    worldApi.backends().then(setBackends).catch(() => {});
  }, []);
  useEffect(() => {
    if (agent.cli !== "omp") return;
    worldApi.ompConfig(agent.id).then((c) => setOmpModels(c.models.map((m) => m.id))).catch(() => {});
  }, [agent.id, agent.cli]);
  const models = Array.from(new Set([...(agent.cli === "omp" ? ompModels : []), ...(backends[agent.cli]?.models ?? []), ...(agent.cli === "claude" ? ["opus", "sonnet", "haiku"] : [])]));
  const clis = Array.from(new Set([agent.cli, ...Object.keys(backends)]));
  const efforts = EFFORTS[agent.cli];
  const fastable = agent.cli === "claude" || agent.cli === "codex";
  const switchCli = async (cli: string) => {
    if (cli === targetCli || !onAgentUpdated) return;
    setSwitching(true);
    setSwitchError("");
    try {
      const updated = await worldApi.update(agent.id, { cli, model: cli === agent.cli ? agent.model : backends[cli]?.default_model ?? backends[cli]?.models[0] ?? "" });
      onChange({ model: "", effort: "", fast: false });
      onAgentUpdated(updated);
    } catch (error) {
      setSwitchError(error instanceof WorldApiError && error.message ? error.message : "Could not switch the CLI.");
    } finally {
      setSwitching(false);
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-w-line px-2.5 py-2 text-w-xs text-w-muted">
      {onAgentUpdated && clis.length > 1 && (
        <span className="flex" title="Switching CLI moves memory to a new session">
          <WSelect
            size="sm"
            icon={<Terminal size={13} weight="bold" />}
            ariaLabel="Coding CLI"
            className="w-auto max-w-[15rem] whitespace-nowrap"
            value={targetCli}
            disabled={switching}
            onChange={(v) => void switchCli(v)}
            options={clis.map((c) => ({ value: c, label: cliName(c), hint: c === agent.cli ? "now" : c === targetCli ? "next" : undefined }))}
          />
        </span>
      )}
      {switchError && <span className="text-w-danger">{switchError}</span>}
      <div className="w-48 max-w-full">
        <WCombobox
          ariaLabel="Model for this message"
          value={options.model ?? ""}
          onChange={(v) => onChange({ model: v === agent.model ? "" : v })}
          placeholder={modelName(agent.model)}
          options={models.map((m) => ({ value: m, label: modelName(m), hint: m === agent.model ? "their usual" : undefined }))}
          placeholderIsValue
          className="h-8 rounded-md px-2 text-w-xs"
        />
      </div>
      {efforts && (
        <span className="flex">
          <WSelect
            size="sm"
            icon={<Brain size={13} weight="bold" />}
            ariaLabel="Reasoning effort"
            className="w-auto max-w-[15rem] whitespace-nowrap"
            value={options.effort ?? ""}
            onChange={(v) => onChange({ effort: v })}
            options={[{ value: "", label: "Auto" }, ...efforts.map((e) => ({ value: e, label: e }))]}
          />
        </span>
      )}
      {fastable && (
        <button
          type="button"
          role="switch"
          aria-checked={Boolean(options.fast)}
          onClick={() => onChange({ fast: !options.fast })}
          title="Faster answers, higher price"
          className={cn(
            "inline-flex h-8 items-center gap-1.5 rounded-md border-2 border-w-ink px-2 text-w-xs transition-colors duration-150",
            FOCUS,
            "focus-visible:ring-offset-0",
            options.fast ? "bg-w-honey text-w-strong" : "bg-w-field text-w-text hover:bg-w-paper",
          )}
        >
          <Lightning size={13} weight={options.fast ? "fill" : "bold"} />
          Fast
        </button>
      )}
      {(options.model || options.effort || options.fast) && (
        <button type="button" onClick={() => onChange({ model: "", effort: "", fast: false })} className="underline hover:text-w-text">
          Reset
        </button>
      )}
    </div>
  );
}

export type AsideChoice = { cli?: string; model?: string };

export function useAsideChoice(agent: WorldAgent) {
  const key = `world.aside.${agent.id}`;
  const [choice, setChoice] = useState<AsideChoice>({});
  useEffect(() => {
    try {
      const raw = window.localStorage.getItem(key);
      setChoice(raw ? (JSON.parse(raw) as AsideChoice) : {});
    } catch {
      setChoice({});
    }
  }, [key]);
  const update = (next: AsideChoice) => {
    setChoice(next);
    try {
      window.localStorage.setItem(key, JSON.stringify(next));
    } catch {}
  };
  const effective: AsideChoice | undefined = choice.model || (choice.cli && choice.cli !== agent.cli) ? choice : undefined;
  return { choice, update, effective };
}

export function AsideControls({ agent, choice, onChange }: { agent: WorldAgent; choice: AsideChoice; onChange: (next: AsideChoice) => void }) {
  const [backends, setBackends] = useState<Record<string, BackendModels>>({});
  useEffect(() => {
    worldApi.backends().then(setBackends).catch(() => {});
  }, []);
  const cli = choice.cli || agent.cli;
  const own = cli === agent.cli;
  const models = Array.from(new Set([...(backends[cli]?.models ?? []), ...(cli === "claude" ? ["haiku", "sonnet", "opus"] : [])]));
  const clis = Array.from(new Set([agent.cli, ...Object.keys(backends)]));
  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-w-line px-2.5 py-2 text-w-xs text-w-muted">
      <span className="flex items-center gap-1" title="Another CLI starts from notes, not full memory">
        <Terminal size={14} weight="bold" />
        <WSelect
          size="sm"
          ariaLabel="CLI for this aside"
          className="w-auto max-w-[15rem] whitespace-nowrap"
          value={cli}
          onChange={(v) => onChange({ cli: v === agent.cli ? undefined : v, model: v === agent.cli ? undefined : backends[v]?.default_model || backends[v]?.models[0] })}
          options={clis.map((c) => ({ value: c, label: cliName(c), hint: c === agent.cli ? "theirs" : undefined }))}
        />
      </span>
      <div className="w-44">
        <WCombobox
          ariaLabel="Model for this aside"
          value={choice.model ?? ""}
          onChange={(v) => onChange({ ...choice, model: own && v === agent.model ? undefined : v || undefined })}
          placeholder={own ? agent.model : "pick a model"}
          options={models.map((m) => ({ value: m, label: modelName(m), hint: own && m === agent.model ? "their usual" : undefined }))}
          className="h-8 rounded-md px-2 text-w-xs"
        />
      </div>
      <span>{own ? "Same memory, pick a smaller model for a quicker answer." : "Fresh mind with their notes, no full memory."}</span>
      {(choice.cli || choice.model) && (
        <button type="button" onClick={() => onChange({})} className="underline hover:text-w-text">
          Reset
        </button>
      )}
    </div>
  );
}
