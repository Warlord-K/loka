"use client";

import { bindTerminalKeys } from "@/lib/terminal-keys";
import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { CaretDown, Plus, SquareSplitHorizontal, TerminalWindow, Trash } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { PIXEL } from "../styles";
import { wb, type WbTerm } from "./bridge";
import { QuoteButton, type MessageQuote } from "../quote";
import { TERMINAL_THEME, readTerminal, selectionSpot, typeCommand } from "../terminal-surface";
import type { TerminalReader } from "../terminal";

const PLACE = "the Workshop terminal";

interface Live {
  since: number;
  info: WbTerm;
  term: Terminal;
  fit: FitAddon;
  host: HTMLDivElement;
  observer: ResizeObserver;
  exited: boolean;
}

export function TerminalPane({
  sessionKey,
  onHide,
  onError,
  readerRef,
  request = null,
  onRequestTaken,
  onQuote,
}: {
  sessionKey: string;
  onHide: () => void;
  onError: (message: string) => void;
  readerRef?: MutableRefObject<TerminalReader | null>;
  request?: { id: number; command: string } | null;
  onRequestTaken?: (id: number) => void;
  onQuote?: (quote: MessageQuote) => void;
}) {
  const live = useRef(new Map<string, Live>());
  const [list, setList] = useState<WbTerm[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [second, setSecond] = useState<string | null>(null);
  const [exited, setExited] = useState<Set<string>>(new Set());
  const slotA = useRef<HTMLDivElement>(null);
  const slotB = useRef<HTMLDivElement>(null);
  const wantFocus = useRef(false);
  const activeRef = useRef<string | null>(null);
  activeRef.current = active;
  const [quoteSpot, setQuoteSpot] = useState<{ x: number; y: number } | null>(null);
  const handled = useRef<number | null>(null);
  const pick = (id: string) => {
    wantFocus.current = true;
    setActive(id);
  };

  const attach = useCallback(
    async (existing: WbTerm | null) => {
      const host = document.createElement("div");
      host.className = "h-full w-full";
      const term = new Terminal({
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        fontSize: 12,
        lineHeight: 1.15,
        cursorBlink: true,
        scrollback: 8000,
        allowProposedApi: true,
        theme: TERMINAL_THEME,
        macOptionIsMeta: true,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      bindTerminalKeys(term);
      const parking = document.createElement("div");
      parking.className = "fixed -left-[9999px] top-0 h-[300px] w-[800px]";
      document.body.appendChild(parking);
      parking.appendChild(host);
      term.open(host);
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        term.loadAddon(webgl);
      } catch {}
      fit.fit();
      let id = existing?.id ?? "";
      const pending: string[] = [];
      try {
        const info = await wb.termOpen(sessionKey, term.cols, term.rows, existing?.id ?? null, (event) => {
          if (event.type === "data") {
            if (id) term.write(event.data);
            else pending.push(event.data);
          } else {
            term.write("\r\n\x1b[2m[process exited]\x1b[0m\r\n");
            const known = id;
            const item = known ? live.current.get(known) : undefined;
            if (item) item.exited = true;
            if (known) setExited((prev) => new Set(prev).add(known));
          }
        });
        id = info.id;
        for (const chunk of pending) term.write(chunk);
        term.onData((data) => void wb.termWrite(id, data).catch(() => {}));
        term.onResize(({ cols, rows }) => void wb.termResize(id, cols, rows).catch(() => {}));
        const observer = new ResizeObserver(() => {
          if (host.isConnected && host.offsetParent !== null && host.clientWidth > 0) {
            try {
              fit.fit();
            } catch {}
          }
        });
        observer.observe(host);
        term.onSelectionChange(() => {
          if (activeRef.current !== id) return;
          setQuoteSpot(term.hasSelection() && term.getSelection().trim() ? selectionSpot(term, host.querySelector(".xterm-screen")) : null);
        });
        term.onScroll(() => {
          if (activeRef.current === id) setQuoteSpot(null);
        });
        live.current.set(id, { since: Date.now(), info, term, fit, host, observer, exited: info.exited });
        if (info.exited) setExited((prev) => new Set(prev).add(id));
        parking.remove();
        return info;
      } catch (error) {
        term.dispose();
        parking.remove();
        onError(error instanceof Error ? error.message : "Could not start a terminal.");
        return null;
      }
    },
    [sessionKey, onError],
  );

  const addTerm = useCallback(async () => {
    const info = await attach(null);
    if (!info) return null;
    setList((prev) => [...prev, info]);
    pick(info.id);
    return info;
  }, [attach]);

  useEffect(() => {
    let cancelled = false;
    const map = live.current;
    const drop = (id: string) => {
      const item = map.get(id);
      item?.observer.disconnect();
      item?.term.dispose();
      map.delete(id);
    };
    void (async () => {
      const existing = await wb.termList(sessionKey).catch(() => [] as WbTerm[]);
      if (cancelled) return;
      const attached: WbTerm[] = [];
      for (const item of existing) {
        const info = await attach(item);
        if (cancelled) {
          if (info) drop(info.id);
          return;
        }
        if (info) attached.push(info);
      }
      if (attached.length) {
        setList(attached);
        setActive(attached[attached.length - 1].id);
        return;
      }
      const info = await attach(null);
      if (cancelled) {
        if (info) drop(info.id);
        return;
      }
      if (info) {
        setList([info]);
        setActive(info.id);
      }
    })();
    return () => {
      cancelled = true;
      for (const id of [...map.keys()]) drop(id);
    };
  }, [sessionKey, attach]);

  useEffect(() => {
    const place = (slot: HTMLDivElement | null, id: string | null) => {
      if (!slot) return;
      const item = id ? live.current.get(id) : undefined;
      if (item && item.host.parentElement !== slot) {
        slot.replaceChildren(item.host);
      } else if (!item) slot.replaceChildren();
      if (item) {
        requestAnimationFrame(() => {
          try {
            item.fit.fit();
          } catch {}
        });
      }
    };
    place(slotA.current, active);
    place(slotB.current, second && second !== active ? second : null);
    if (active && wantFocus.current) {
      wantFocus.current = false;
      requestAnimationFrame(() => live.current.get(active)?.term.focus());
    }
  }, [active, second, list]);

  useEffect(() => {
    setQuoteSpot(null);
  }, [active]);

  useEffect(() => {
    if (!readerRef) return;
    readerRef.current = () => {
      const item = activeRef.current ? live.current.get(activeRef.current) : undefined;
      return item ? readTerminal(item.term, false, PLACE) : null;
    };
    return () => {
      readerRef.current = null;
    };
  }, [readerRef]);

  useEffect(() => {
    if (!request || handled.current === request.id || !active) return;
    const item = live.current.get(active);
    if (!item) return;
    handled.current = request.id;
    onRequestTaken?.(request.id);
    const id = active;
    void typeCommand(item.term, (data) => void wb.termWrite(id, data).catch(() => {}), request.command, {
      liveSince: item.since,
      alive: () => live.current.get(id) === item && !item.exited,
    }).then((result) => {
      if (result === "copied") onError("This shell does not take pasted lines, so the command was copied instead. Paste it at the prompt.");
      else if (result === "not_copied") onError("This shell does not take pasted lines. Copy the command from the chat and paste it at the prompt.");
    });
  }, [request, active, list, onError, onRequestTaken]);

  const quoteSelection = () => {
    const item = active ? live.current.get(active) : undefined;
    setQuoteSpot(null);
    if (!item || !onQuote) return;
    const quote = readTerminal(item.term, true, PLACE);
    if (!quote) return;
    item.term.clearSelection();
    onQuote(quote);
  };

  const close = async (id: string) => {
    await wb.termClose(id).catch(() => {});
    const item = live.current.get(id);
    item?.observer.disconnect();
    item?.term.dispose();
    live.current.delete(id);
    const rest = list.filter((t) => t.id !== id);
    setList(rest);
    if (second === id) setSecond(null);
    if (active === id) setActive(rest.length ? rest[rest.length - 1].id : null);
    if (!rest.length) onHide();
  };

  const split = async () => {
    if (second && second !== active) {
      setSecond(null);
      return;
    }
    const current = active;
    const info = await addTerm();
    if (info && current) {
      setActive(current);
      setSecond(info.id);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-[#120c15]">
      <div className="flex items-center gap-2 border-b border-[#3a2a3d] bg-[#1d1422] px-2 py-1">
        <span className={cn(PIXEL, "text-w-xs text-[#e9dcc6]")}>Terminal</span>
        <span className="flex-1" />
        <button type="button" title="New terminal" aria-label="New terminal" onClick={() => void addTerm()} className="text-[#9c8a9a] hover:text-white">
          <Plus size={14} weight="bold" />
        </button>
        <button type="button" title="Split terminal" aria-label="Split terminal" aria-pressed={!!second} onClick={() => void split()} className={cn("hover:text-white", second ? "text-[#e8b64a]" : "text-[#9c8a9a]")}>
          <SquareSplitHorizontal size={14} />
        </button>
        <button type="button" title="Kill terminal" aria-label="Kill terminal" disabled={!active} onClick={() => active && void close(active)} className="text-[#9c8a9a] hover:text-[#e07a5f] disabled:opacity-40">
          <Trash size={14} />
        </button>
        <button type="button" title="Hide panel" aria-label="Hide terminal panel" onClick={onHide} className="text-[#9c8a9a] hover:text-white">
          <CaretDown size={14} weight="bold" />
        </button>
      </div>
      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1">
          <div ref={slotA} className="min-w-0 flex-1 px-1.5 pt-1" />
          <QuoteButton spot={onQuote ? quoteSpot : null} onQuote={quoteSelection} />
          <div ref={slotB} className={cn("min-w-0 flex-1 border-l border-[#3a2a3d] px-1.5 pt-1", !(second && second !== active) && "hidden")} />
        </div>
        <ul className="w-36 shrink-0 overflow-y-auto border-l border-[#3a2a3d] bg-[#17101b] py-1 text-w-xs">
          {list.map((t) => (
            <li key={t.id}>
              <button
                type="button"
                onClick={() => pick(t.id)}
                className={cn(
                  "flex w-full items-center gap-1.5 px-2 py-1 text-left hover:bg-[#241a29]",
                  (t.id === active || t.id === second) && "bg-[#2c2032] text-white",
                  t.id !== active && t.id !== second && "text-[#9c8a9a]",
                )}
              >
                <TerminalWindow size={12} />
                <span className="min-w-0 flex-1 truncate">{t.title}</span>
                {exited.has(t.id) && <span title="Exited" className="h-1.5 w-1.5 rounded-full bg-[#e07a5f]" />}
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
