"use client";

import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { history, historyKeymap, defaultKeymap, indentWithTab } from "@codemirror/commands";
import { bracketMatching, foldGutter, foldKeymap, HighlightStyle, indentOnInput, syntaxHighlighting } from "@codemirror/language";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { sql } from "@codemirror/lang-sql";
import { yaml } from "@codemirror/lang-yaml";
import { goToNextChunk, goToPreviousChunk, MergeView, unifiedMergeView } from "@codemirror/merge";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { EditorState, Prec, type Extension, type Text, type Transaction } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
  type ViewUpdate,
} from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { vscodeKeymap } from "@replit/codemirror-vscode-keymap";
import { CaretRight, Columns, Rows, X } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { wb, type WbChange } from "./bridge";
import { baseName, STATUS_COLOR, STATUS_WORD } from "./file-tree";

export type TabMode = "code" | "diff";
export interface Tab {
  path: string;
  mode: TabMode;
}

interface Doc {
  path: string;
  state: EditorState | null;
  saved: Text | null;
  sha: string | null;
  dirty: boolean;
  diskChanged: boolean;
  conflict: boolean;
  missing: boolean;
  binary: boolean;
  tooLarge: boolean;
  loading: boolean;
  error: string | null;
  base: string | null | undefined;
  language: string;
  rev: number;
}

export interface EditorHandle {
  dirtyPaths: () => string[];
  saveActive: () => void;
  reveal: (path: string, line: number, column: number) => void;
}

const LANGUAGES: [RegExp, string, () => Extension][] = [
  [/\.(tsx)$/i, "TypeScript JSX", () => javascript({ jsx: true, typescript: true })],
  [/\.(ts|mts|cts)$/i, "TypeScript", () => javascript({ typescript: true })],
  [/\.(jsx)$/i, "JavaScript JSX", () => javascript({ jsx: true })],
  [/\.(js|mjs|cjs)$/i, "JavaScript", () => javascript()],
  [/\.rs$/i, "Rust", () => rust()],
  [/\.(py|pyi)$/i, "Python", () => python()],
  [/\.(json|jsonc|webmanifest)$/i, "JSON", () => json()],
  [/\.(md|mdx|markdown)$/i, "Markdown", () => markdown()],
  [/\.(css|scss|less)$/i, "CSS", () => css()],
  [/\.sql$/i, "SQL", () => sql()],
  [/\.(ya?ml)$/i, "YAML", () => yaml()],
  [/\.(html?|svg|xml)$/i, "HTML", () => html()],
];

export function languageFor(path: string): { name: string; extension: Extension } {
  const found = LANGUAGES.find(([re]) => re.test(path));
  return found ? { name: found[1], extension: found[2]() } : { name: "Plain text", extension: [] };
}

const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.moduleKeyword, t.operatorKeyword], color: "#c39bff" },
  { tag: [t.string, t.special(t.string), t.regexp], color: "#9bf08b" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "#f2a35a" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: "#7d6b7b", fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "#6bc7e8" },
  { tag: [t.typeName, t.className, t.namespace], color: "#e8b64a" },
  { tag: [t.propertyName, t.attributeName], color: "#e9c98a" },
  { tag: [t.tagName, t.angleBracket], color: "#ff8fa8" },
  { tag: [t.definition(t.variableName)], color: "#f3e7d3" },
  { tag: [t.heading], color: "#e8b64a", fontWeight: "bold" },
  { tag: [t.link, t.url], color: "#6bc7e8", textDecoration: "underline" },
  { tag: [t.emphasis], fontStyle: "italic" },
  { tag: [t.strong], fontWeight: "bold" },
  { tag: [t.invalid], color: "#e07a5f" },
]);

const theme = EditorView.theme(
  {
    "&": { height: "100%", backgroundColor: "#17101b", color: "#e9dcc6", fontSize: "12.5px" },
    ".cm-scroller": { fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace", lineHeight: "1.55" },
    ".cm-content": { caretColor: "#e8b64a" },
    ".cm-cursor, .cm-dropCursor": { borderLeftColor: "#e8b64a", borderLeftWidth: "2px" },
    "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": { backgroundColor: "#3d2f4a !important" },
    ".cm-gutters": { backgroundColor: "#17101b", color: "#5f4f60", border: "none" },
    ".cm-activeLineGutter": { backgroundColor: "#1f1724", color: "#cbb9a0" },
    ".cm-activeLine": { backgroundColor: "#1f172466" },
    ".cm-foldPlaceholder": { backgroundColor: "#33243a", border: "none", color: "#e9dcc6" },
    ".cm-selectionMatch": { backgroundColor: "#4a3a2a88" },
    ".cm-matchingBracket": { backgroundColor: "#3d2f4a", outline: "1px solid #7d6b7b" },
    ".cm-searchMatch": { backgroundColor: "#e8b64a44", outline: "1px solid #e8b64a" },
    ".cm-panels": { backgroundColor: "#1d1422", color: "#e9dcc6", borderColor: "#3a2a3d" },
    ".cm-panel input, .cm-panel button": { color: "#e9dcc6", backgroundColor: "#140d18", border: "1px solid #3a2a3d", borderRadius: "4px" },
    ".cm-tooltip": { backgroundColor: "#221827", border: "1px solid #3a2a3d", color: "#e9dcc6" },
    ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "#33243a", color: "#fff" },
    "&.cm-merge-b .cm-changedLine, .cm-insertedLine": { backgroundColor: "#2f6b2a33" },
    "&.cm-merge-b .cm-changedText, .cm-insertedLine .cm-changedText": { background: "#5fd05055" },
    "&.cm-merge-a .cm-changedLine, .cm-deletedChunk": { backgroundColor: "#8a2f2f33" },
    "&.cm-merge-a .cm-changedText, .cm-deletedChunk .cm-deletedText": { background: "#e07a5f55" },
    ".cm-changeGutter": { width: "3px", paddingLeft: "1px" },
    "&.cm-merge-b .cm-changedLineGutter, .cm-insertedLineGutter": { background: "#5fd050" },
    "&.cm-merge-a .cm-changedLineGutter, .cm-deletedLineGutter": { background: "#e07a5f" },
    ".cm-collapsedLines": { backgroundColor: "#221827", color: "#9c8a9a" },
  },
  { dark: true },
);

const mergeTheme = EditorView.theme({
  "&.cm-mergeView, .cm-mergeView": { height: "100%" },
  ".cm-mergeViewEditors": { minHeight: "100%" },
  ".cm-mergeSpacer": { backgroundImage: "repeating-linear-gradient(-45deg, #1f1724 0 4px, #17101b 4px 8px)" },
});

function baseSetup(language: Extension, onUpdate: (update: ViewUpdate) => void, onSave: () => void, readOnly = false): Extension {
  return [
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    history(),
    foldGutter(),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    EditorState.readOnly.of(readOnly),
    indentOnInput(),
    syntaxHighlighting(highlight, { fallback: true }),
    bracketMatching(),
    closeBrackets(),
    autocompletion(),
    rectangularSelection(),
    crosshairCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    Prec.highest(keymap.of([{ key: "Mod-s", preventDefault: true, run: () => (onSave(), true) }, { key: "Alt-ArrowDown", run: goToNextChunk }, { key: "Alt-ArrowUp", run: goToPreviousChunk }])),
    keymap.of([...vscodeKeymap, ...closeBracketsKeymap, ...searchKeymap, ...historyKeymap, ...foldKeymap, ...completionKeymap, ...defaultKeymap, indentWithTab]),
    language,
    theme,
    EditorView.updateListener.of(onUpdate),
  ];
}

function fresh(path: string): Doc {
  return {
    path,
    state: null,
    saved: null,
    sha: null,
    dirty: false,
    diskChanged: false,
    conflict: false,
    missing: false,
    binary: false,
    tooLarge: false,
    loading: true,
    error: null,
    base: undefined,
    language: languageFor(path).name,
    rev: 0,
  };
}

export const EditorPane = forwardRef<
  EditorHandle,
  {
    sessionKey: string;
    base: string | null;
    tabs: Tab[];
    active: string | null;
    changes: WbChange[];
    refresh: { nonce: number; paths: string[] };
    split: boolean;
    onSplitChange: (split: boolean) => void;
    onActivate: (path: string) => void;
    onClose: (path: string) => void;
    onModeChange: (path: string, mode: TabMode) => void;
    onCursor: (info: { line: number; column: number; language: string } | null) => void;
    onDirtyChange: (paths: string[]) => void;
    onSaved: () => void;
    onError: (message: string) => void;
  }
>(function EditorPane(props, ref) {
  const { sessionKey, base, tabs, active, changes, refresh, split, onSplitChange, onActivate, onClose, onModeChange, onError } = props;
  const docs = useRef(new Map<string, Doc>());
  const [, setVersion] = useState(0);
  const bump = useCallback(() => setVersion((v) => v + 1), []);
  const codeBox = useRef<HTMLDivElement>(null);
  const diffBox = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const shown = useRef<string | null>(null);
  const diff = useRef<{ destroy: () => void; path: string; focus: () => void } | null>(null);
  const activeRef = useRef(active);
  activeRef.current = active;
  const propsRef = useRef(props);
  propsRef.current = props;

  const reportDirty = useCallback(() => {
    propsRef.current.onDirtyChange([...docs.current.values()].filter((d) => d.dirty).map((d) => d.path));
  }, []);

  const lastCursor = useRef("");
  const reportCursor = useCallback((state: EditorState | null, doc: Doc | null) => {
    if (!state || !doc) {
      if (lastCursor.current) propsRef.current.onCursor(null);
      lastCursor.current = "";
      return;
    }
    const head = state.selection.main.head;
    const line = state.doc.lineAt(head);
    const info = { line: line.number, column: head - line.from + 1, language: doc.language };
    const key = `${info.line}:${info.column}:${info.language}`;
    if (key === lastCursor.current) return;
    lastCursor.current = key;
    propsRef.current.onCursor(info);
  }, []);

  const afterChange = useCallback(
    (doc: Doc) => {
      const dirty = !!doc.state && !!doc.saved && !doc.state.doc.eq(doc.saved);
      if (dirty !== doc.dirty) {
        doc.dirty = dirty;
        bump();
        reportDirty();
      }
    },
    [bump, reportDirty],
  );

  const save = useCallback(
    async (path: string | null, force = false) => {
      const doc = path ? docs.current.get(path) : undefined;
      if (!doc?.state || doc.binary || doc.tooLarge) return;
      const text = doc.state.doc.toString();
      try {
        let sha = doc.missing ? null : doc.sha;
        if (force) sha = (await wb.read(sessionKey, doc.path).catch(() => null))?.sha ?? null;
        const result = await wb.write(sessionKey, doc.path, text, sha);
        if (result.conflict) {
          doc.conflict = true;
          bump();
          return;
        }
        doc.sha = result.sha;
        doc.saved = doc.state.doc;
        doc.missing = false;
        doc.conflict = false;
        doc.diskChanged = false;
        afterChange(doc);
        bump();
        propsRef.current.onSaved();
      } catch (error) {
        onError(error instanceof Error ? error.message : "Could not save.");
      }
    },
    [sessionKey, bump, afterChange, onError],
  );

  const saveRef = useRef(save);
  saveRef.current = save;

  const makeState = useCallback(
    (doc: Doc, text: string) =>
      EditorState.create({
        doc: text,
        extensions: baseSetup(
          languageFor(doc.path).extension,
          (update) => {
            if (shown.current !== doc.path || propsRef.current.tabs.find((tab) => tab.path === doc.path)?.mode === "diff") return;
            doc.state = update.state;
            if (update.docChanged) afterChange(doc);
            if (update.selectionSet || update.docChanged || update.focusChanged) reportCursor(update.state, doc);
          },
          () => void saveRef.current(doc.path),
        ),
      }),
    [afterChange, reportCursor],
  );

  const load = useCallback(
    async (doc: Doc, quiet = false) => {
      try {
        const file = await wb.read(sessionKey, doc.path);
        doc.loading = false;
        doc.binary = file.binary;
        doc.tooLarge = file.too_large;
        doc.missing = false;
        doc.error = null;
        if (file.text === null) {
          doc.state = null;
          bump();
          return;
        }
        if (quiet && doc.sha === file.sha) return;
        doc.sha = file.sha;
        if (doc.state && quiet) {
          const tr = doc.state.update({ changes: { from: 0, to: doc.state.doc.length, insert: file.text } });
          if (view.current && shown.current === doc.path && view.current.state === doc.state) view.current.dispatch(tr);
          doc.state = tr.state;
        } else {
          doc.state = makeState(doc, file.text);
        }
        doc.saved = doc.state.doc;
        doc.diskChanged = false;
        doc.conflict = false;
        doc.rev += 1;
        afterChange(doc);
        bump();
      } catch (error) {
        doc.loading = false;
        const message = error instanceof Error ? error.message : "Could not open it.";
        if (/gone/i.test(message)) {
          doc.missing = true;
          doc.state = makeState(doc, "");
          doc.saved = doc.state.doc;
          doc.sha = null;
        } else doc.error = message;
        bump();
      }
    },
    [sessionKey, bump, makeState, afterChange],
  );

  useEffect(() => {
    const open = new Set(tabs.map((tab) => tab.path));
    for (const path of [...docs.current.keys()]) if (!open.has(path)) docs.current.delete(path);
    for (const tab of tabs) {
      if (!docs.current.has(tab.path)) {
        const doc = fresh(tab.path);
        docs.current.set(tab.path, doc);
        void load(doc);
      }
    }
    reportDirty();
  }, [tabs, load, reportDirty]);

  useEffect(() => {
    if (!refresh.nonce) return;
    const touched = new Set(refresh.paths);
    for (const doc of docs.current.values()) {
      if (!touched.has(doc.path)) continue;
      if (doc.dirty) {
        wb.read(sessionKey, doc.path)
          .then((file) => {
            if (file.sha !== doc.sha) {
              doc.diskChanged = true;
              bump();
            }
          })
          .catch(() => {
            doc.diskChanged = true;
            bump();
          });
      } else void load(doc, true);
      doc.base = undefined;
    }
  }, [refresh, sessionKey, load, bump]);

  const activeTab = tabs.find((tab) => tab.path === active) ?? null;
  const activeDoc = active ? docs.current.get(active) ?? null : null;
  const mode = activeTab?.mode ?? "code";
  const change = changes.find((c) => c.path === active) ?? null;

  useEffect(() => {
    if (!codeBox.current || view.current) return;
    view.current = new EditorView({ parent: codeBox.current, state: EditorState.create({ doc: "", extensions: [theme, EditorState.readOnly.of(true)] }) });
    return () => {
      view.current?.destroy();
      view.current = null;
    };
  }, []);

  const focusedPath = useRef<string | null>(null);
  const hasState = !!activeDoc?.state;
  const activeRev = activeDoc?.rev ?? 0;
  useEffect(() => {
    const v = view.current;
    const doc = active ? docs.current.get(active) : undefined;
    if (!v) return;
    if (!doc?.state || mode !== "code") {
      shown.current = null;
      if (!doc) reportCursor(null, null);
      if (mode !== "code") focusedPath.current = null;
      return;
    }
    if (v.state !== doc.state) v.setState(doc.state);
    shown.current = doc.path;
    reportCursor(doc.state, doc);
    if (focusedPath.current !== doc.path) {
      focusedPath.current = doc.path;
      requestAnimationFrame(() => v.focus());
    }
  }, [active, mode, hasState, activeRev, reportCursor]);

  const fetchingBase = useRef(new Set<string>());
  const ensureBase = useCallback(
    async (doc: Doc) => {
      if (doc.base !== undefined || fetchingBase.current.has(doc.path)) return;
      fetchingBase.current.add(doc.path);
      const source = changes.find((c) => c.path === doc.path)?.old_path ?? doc.path;
      try {
        const result = await wb.base(sessionKey, source, base);
        doc.base = result.exists && result.text !== null ? result.text : null;
      } catch {
        doc.base = null;
      }
      fetchingBase.current.delete(doc.path);
      bump();
    },
    [changes, sessionKey, base, bump],
  );

  const baseMissing = activeDoc?.base === undefined;
  const activeLoading = activeDoc?.loading ?? true;
  useEffect(() => {
    const doc = active ? docs.current.get(active) : undefined;
    if (mode === "diff" && doc && !doc.loading) void ensureBase(doc);
  }, [active, mode, baseMissing, activeLoading, ensureBase]);

  const baseText = activeDoc?.base;
  const docReady = !!activeDoc?.state && !activeDoc.loading;
  const rev = activeDoc?.rev ?? 0;
  useEffect(() => {
    diff.current?.destroy();
    diff.current = null;
    const box = diffBox.current;
    const doc = activeDoc;
    if (!box || mode !== "diff" || !doc?.state || baseText === undefined || !docReady) return;
    const language = languageFor(doc.path).extension;
    const sync = (trs: readonly Transaction[], state: EditorState) => {
      for (const tr of trs) if (tr.docChanged && doc.state) doc.state = doc.state.update({ changes: tr.changes }).state;
      afterChange(doc);
      reportCursor(state, doc);
    };
    const onUpdate = (update: ViewUpdate) => {
      if (update.docChanged) sync(update.transactions, update.state);
      else if (update.selectionSet) reportCursor(update.state, doc);
    };
    const onSave = () => void saveRef.current(doc.path);
    const text = doc.state.doc.toString();
    const original = baseText ?? "";
    if (split) {
      const merge = new MergeView({
        a: { doc: original, extensions: [baseSetup(language, () => {}, onSave, true), EditorView.editable.of(false), mergeTheme] },
        b: { doc: text, extensions: [baseSetup(language, onUpdate, onSave), mergeTheme] },
        parent: box,
        gutter: true,
        highlightChanges: true,
        collapseUnchanged: { margin: 4, minSize: 10 },
      });
      merge.dom.classList.add("h-full");
      diff.current = { destroy: () => merge.destroy(), path: doc.path, focus: () => merge.b.focus() };
      requestAnimationFrame(() => merge.b.focus());
    } else {
      const v = new EditorView({
        parent: box,
        state: EditorState.create({
          doc: text,
          extensions: [baseSetup(language, onUpdate, onSave), unifiedMergeView({ original, gutter: true, highlightChanges: true, mergeControls: false })],
        }),
      });
      diff.current = { destroy: () => v.destroy(), path: doc.path, focus: () => v.focus() };
      requestAnimationFrame(() => v.focus());
    }
    return () => {
      diff.current?.destroy();
      diff.current = null;
    };
  }, [mode, active, split, baseText, docReady, rev, afterChange, reportCursor]);

  useImperativeHandle(
    ref,
    () => ({
      dirtyPaths: () => [...docs.current.values()].filter((d) => d.dirty).map((d) => d.path),
      saveActive: () => void saveRef.current(activeRef.current),
      reveal: (path, line, column) => {
        const go = (attempt = 0) => {
          const doc = docs.current.get(path);
          const v = view.current;
          if (!doc?.state || !v || shown.current !== path) {
            if (attempt < 40) setTimeout(() => go(attempt + 1), 50);
            return;
          }
          const target = doc.state.doc.line(Math.min(Math.max(1, line), doc.state.doc.lines));
          const pos = Math.min(target.to, target.from + Math.max(0, column - 1));
          v.dispatch({ selection: { anchor: pos }, effects: EditorView.scrollIntoView(pos, { y: "center" }) });
          v.focus();
        };
        go();
      },
    }),
    [],
  );

  const close = (path: string) => {
    const doc = docs.current.get(path);
    if (doc?.dirty && !window.confirm(`${baseName(path)} has unsaved changes. Close it anyway?`)) return;
    onClose(path);
  };

  const resolveConflict = async (keepMine: boolean) => {
    if (!activeDoc) return;
    if (keepMine) await save(activeDoc.path, true);
    else {
      activeDoc.state = null;
      activeDoc.dirty = false;
      await load(activeDoc);
      reportDirty();
    }
  };

  const crumbs = active ? active.split("/") : [];

  return (
    <div className="flex h-full min-h-0 flex-col bg-[#17101b]">
      <div className="flex min-h-[2.1rem] items-stretch overflow-x-auto border-b border-[#3a2a3d] bg-[#1d1422] [scrollbar-width:none]">
        {tabs.map((tab) => {
          const doc = docs.current.get(tab.path);
          const letter = changes.find((c) => c.path === tab.path)?.status;
          return (
            <div
              key={tab.path}
              role="tab"
              aria-selected={tab.path === active}
              onClick={() => onActivate(tab.path)}
              onAuxClick={(e) => e.button === 1 && close(tab.path)}
              title={tab.path}
              className={cn(
                "group flex max-w-[14rem] shrink-0 cursor-pointer items-center gap-1.5 border-r border-[#3a2a3d] px-3 text-w-xs",
                tab.path === active ? "border-t-2 border-t-[#e8b64a] bg-[#17101b] text-[#f3e7d3]" : "text-[#9c8a9a] hover:bg-[#241a29]",
              )}
            >
              {tab.mode === "diff" && <span className="rounded-md bg-[#33243a] px-1 text-w-3xs uppercase text-[#e8b64a]">diff</span>}
              <span className={cn("truncate", letter && STATUS_COLOR[letter])}>{baseName(tab.path)}</span>
              {letter && <span className={cn("font-mono text-w-3xs font-bold", STATUS_COLOR[letter])}>{letter}</span>}
              {doc?.diskChanged && <span title="Changed on disk" className="h-1.5 w-1.5 rounded-full bg-[#6bc7e8]" />}
              <button
                type="button"
                aria-label={`Close ${baseName(tab.path)}`}
                onClick={(e) => {
                  e.stopPropagation();
                  close(tab.path);
                }}
                className="grid h-4 w-4 place-items-center rounded-md text-[#9c8a9a] hover:bg-[#33243a] hover:text-white"
              >
                {doc?.dirty ? <span className="h-2 w-2 rounded-full bg-[#e9dcc6] group-hover:hidden" /> : null}
                <X size={10} weight="bold" className={cn(doc?.dirty && "hidden group-hover:block")} />
              </button>
            </div>
          );
        })}
      </div>
      {active && (
        <div className="flex items-center gap-2 border-b border-[#2a1f2f] px-3 py-1 text-w-xs text-[#9c8a9a]">
          <span className="flex min-w-0 flex-1 items-center gap-1 truncate">
            {crumbs.map((part, i) => (
              <span key={i} className="flex items-center gap-1">
                {i > 0 && <CaretRight size={9} />}
                <span className={cn(i === crumbs.length - 1 && "text-[#e9dcc6]")}>{part}</span>
              </span>
            ))}
            {change && (
              <span className={cn("ml-2 rounded-md bg-[#241a29] px-1.5 text-w-2xs", STATUS_COLOR[change.status])}>
                {STATUS_WORD[change.status]}
                {change.old_path ? ` from ${change.old_path}` : ""}
              </span>
            )}
          </span>
          {mode === "diff" && (
            <span className="flex overflow-hidden rounded-md border border-[#3a2a3d]">
              <button type="button" title="Side by side" aria-pressed={split} onClick={() => onSplitChange(true)} className={cn("px-1.5 py-0.5", split ? "bg-[#33243a] text-white" : "hover:bg-[#241a29]")}>
                <Columns size={12} />
              </button>
              <button type="button" title="Inline" aria-pressed={!split} onClick={() => onSplitChange(false)} className={cn("px-1.5 py-0.5", !split ? "bg-[#33243a] text-white" : "hover:bg-[#241a29]")}>
                <Rows size={12} />
              </button>
            </span>
          )}
          <span className="flex overflow-hidden rounded-md border border-[#3a2a3d]">
            {(["code", "diff"] as const).map((m) => (
              <button key={m} type="button" aria-pressed={mode === m} onClick={() => onModeChange(active, m)} className={cn("px-2 py-0.5 capitalize", mode === m ? "bg-[#33243a] text-white" : "hover:bg-[#241a29]")}>
                {m === "code" ? "Code" : "Diff"}
              </button>
            ))}
          </span>
        </div>
      )}
      {activeDoc?.conflict && (
        <div className="flex flex-wrap items-center gap-2 border-b border-[#6b3a2a] bg-[#3a1f1a] px-3 py-1.5 text-w-xs text-[#f3d3c6]">
          <span className="flex-1">This file changed on disk after you opened it. Your edits are not saved yet.</span>
          <button type="button" onClick={() => void resolveConflict(false)} className="rounded-md border border-[#e07a5f] px-2 py-0.5 hover:bg-[#e07a5f]/20">
            Reload
          </button>
          <button type="button" onClick={() => void resolveConflict(true)} className="rounded-md bg-[#e07a5f] px-2 py-0.5 text-[#1d1422] hover:bg-[#ea8c72]">
            Keep mine
          </button>
        </div>
      )}
      {activeDoc?.diskChanged && !activeDoc.conflict && (
        <div className="flex items-center gap-2 border-b border-[#2a4a5a] bg-[#16262e] px-3 py-1 text-w-xs text-[#bfe3f0]">
          <span className="flex-1">Changed on disk while you were editing. Saving will ask before it overwrites.</span>
          <button type="button" onClick={() => void resolveConflict(false)} className="rounded-md border border-[#6bc7e8] px-2 py-0.5 hover:bg-[#6bc7e8]/20">
            Load theirs
          </button>
        </div>
      )}
      <div className="relative min-h-0 flex-1">
        <div ref={codeBox} className={cn("absolute inset-0", (mode !== "code" || !activeDoc?.state) && "invisible")} />
        <div ref={diffBox} className={cn("absolute inset-0 overflow-auto", mode !== "diff" && "hidden")} />
        {!active && (
          <div className="absolute inset-0 grid place-items-center text-center text-w-sm text-[#7d6b7b]">
            <div>
              <p className="font-[family-name:var(--font-pixel)] text-w-md text-[#9c8a9a]">The workbench is ready</p>
              <p className="mt-1">Pick a file, or a change on the left. Cmd+P opens any file, ] and [ (or Option with them) step through changes.</p>
            </div>
          </div>
        )}
        {activeDoc && (activeDoc.loading || activeDoc.binary || activeDoc.tooLarge || activeDoc.error || (mode === "diff" && activeDoc.base === undefined)) && (
          <div className="absolute inset-0 grid place-items-center bg-[#17101b] text-w-sm text-[#9c8a9a]">
            {activeDoc.error ?? (activeDoc.loading || activeDoc.base === undefined ? "Opening..." : activeDoc.binary ? "This is a binary file." : "This file is over 5 MB, too big to open here.")}
          </div>
        )}
        {activeDoc?.missing && mode === "code" && (
          <div className="pointer-events-none absolute right-3 top-2 rounded-md bg-[#3a1f1a] px-2 py-0.5 text-w-2xs text-[#f3d3c6]">Deleted on disk. Saving brings it back.</div>
        )}
      </div>
    </div>
  );
});
