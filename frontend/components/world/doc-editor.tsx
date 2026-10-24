"use client";

import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useEditor, EditorContent, Extension, type Editor } from "@tiptap/react";
import { BubbleMenu } from "@tiptap/react/menus";
import StarterKit from "@tiptap/starter-kit";
import Text from "@tiptap/extension-text";
import Placeholder from "@tiptap/extension-placeholder";
import Image from "@tiptap/extension-image";
import { TaskItem, TaskList } from "@tiptap/extension-list";
import { Table, TableCell, TableHeader, TableRow } from "@tiptap/extension-table";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";
import type { Node as PmNode } from "@tiptap/pm/model";
import { Markdown } from "tiptap-markdown";
import {
  CheckSquare,
  Code,
  CodeBlock,
  FileText,
  Link as LinkIcon,
  ListBullets,
  ListNumbers,
  Minus,
  Quotes,
  Table as TableIcon,
  TextB,
  TextHOne,
  TextHThree,
  TextHTwo,
  TextItalic,
  TextStrikethrough,
  TextT,
  type Icon as PhosphorIcon,
} from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { takeEscape } from "@/lib/escape";
import { MENU } from "./controls";
import type { FocusPoint } from "@/lib/caret";

const WIKI = /\[\[[^\]\n]+\]\]/g;

export const DOC_PROSE = cn(
  "text-w-md leading-[1.75] text-w-text",
  "[&_.ProseMirror]:min-h-[40vh] [&_.ProseMirror]:pb-24 [&_.ProseMirror]:outline-none [&_.ProseMirror]:[caret-color:rgb(var(--w-teal))]",
  "[&_.ProseMirror>*:first-child]:mt-0 [&_p]:my-3 [&_li]:my-1 [&_li>p]:my-0",
  "[&_h1]:mb-2 [&_h1]:mt-9 [&_h1]:text-[1.6rem] [&_h1]:font-bold [&_h1]:leading-tight [&_h1]:tracking-[-0.01em] [&_h1]:text-w-strong",
  "[&_h2]:mb-1.5 [&_h2]:mt-8 [&_h2]:text-[1.3rem] [&_h2]:font-semibold [&_h2]:leading-snug [&_h2]:tracking-[-0.01em] [&_h2]:text-w-strong",
  "[&_h3]:mb-1 [&_h3]:mt-6 [&_h3]:text-w-lg [&_h3]:font-semibold [&_h3]:text-w-strong",
  "[&_h4]:mb-1 [&_h4]:mt-5 [&_h4]:font-semibold [&_h4]:text-w-text2",
  "[&_ul]:my-3 [&_ul]:ml-5 [&_ul]:list-disc [&_ol]:my-3 [&_ol]:ml-5 [&_ol]:list-decimal",
  "[&_ul[data-type=taskList]]:ml-0.5 [&_ul[data-type=taskList]]:list-none",
  "[&_li[data-checked]]:flex [&_li[data-checked]]:items-start [&_li[data-checked]]:gap-2.5",
  "[&_li[data-checked]_p]:my-0 [&_li[data-checked]>label]:mt-[0.3em] [&_li[data-checked]>label]:select-none [&_li[data-checked]>div]:min-w-0 [&_li[data-checked]>div]:flex-1",
  "[&_li[data-checked=true]>div]:text-w-muted [&_li[data-checked=true]>div]:line-through",
  "[&_input[type=checkbox]]:h-4 [&_input[type=checkbox]]:w-4 [&_input[type=checkbox]]:cursor-pointer [&_input[type=checkbox]]:accent-[rgb(var(--w-teal))]",
  "[&_blockquote]:my-4 [&_blockquote]:border-l-4 [&_blockquote]:border-w-line2 [&_blockquote]:pl-3 [&_blockquote]:text-w-text2",
  "[&_code]:rounded-md [&_code]:bg-w-sand [&_code]:px-1 [&_code]:py-px [&_code]:font-mono [&_code]:text-[0.82em] [&_code]:text-w-warn",
  "[&_pre]:my-4 [&_pre]:overflow-x-auto [&_pre]:rounded-lg [&_pre]:border-2 [&_pre]:border-w-line [&_pre]:bg-w-sand/60 [&_pre]:px-4 [&_pre]:py-3 [&_pre]:text-w-sm [&_pre]:leading-relaxed",
  "[&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_pre_code]:text-w-text",
  "[&_a]:cursor-text [&_a]:text-w-link [&_a]:underline [&_a]:decoration-w-teal/50",
  "[&_hr]:my-7 [&_hr]:border-dashed [&_hr]:border-w-line2 [&_hr.ProseMirror-selectednode]:border-w-teal",
  "[&_.tableWrapper]:my-5 [&_.tableWrapper]:overflow-x-auto [&_table]:w-full [&_table]:border-collapse [&_table]:text-w-base",
  "[&_th]:border [&_th]:border-w-line [&_th]:bg-w-sand/60 [&_th]:px-3 [&_th]:py-2 [&_th]:text-left [&_th]:text-w-sm [&_th]:font-semibold [&_th]:text-w-text2",
  "[&_td]:border [&_td]:border-w-line [&_td]:px-3 [&_td]:py-2 [&_td]:align-top [&_td_p]:my-0 [&_th_p]:my-0 [&_.selectedCell]:bg-w-teal/15",
  "[&_img]:my-3 [&_img]:max-h-96 [&_img]:rounded-lg [&_img]:border-2 [&_img]:border-w-ink [&_img.ProseMirror-selectednode]:ring-2 [&_img.ProseMirror-selectednode]:ring-w-teal",
  "[&_.is-empty]:before:pointer-events-none [&_.is-empty]:before:float-left [&_.is-empty]:before:h-0 [&_.is-empty]:before:text-w-faint [&_.is-empty]:before:content-[attr(data-placeholder)]",
  "[&_.wiki-ref]:rounded-md [&_.wiki-ref]:bg-w-gold/20 [&_.wiki-ref]:px-0.5 [&_.wiki-ref]:font-medium [&_.wiki-ref]:text-w-link",
);

const MarkdownText = Text.extend({
  addStorage() {
    return {
      markdown: {
        serialize(state: { text: (s: string, escape?: boolean) => void }, node: PmNode) {
          const text = node.text ?? "";
          let last = 0;
          for (const m of text.matchAll(WIKI)) {
            const at = m.index ?? 0;
            if (at > last) state.text(text.slice(last, at));
            state.text(m[0], false);
            last = at + m[0].length;
          }
          if (last < text.length || !text.length) state.text(text.slice(last));
        },
        parse: {},
      },
    };
  },
});

const TightTaskList = TaskList.extend({
  addAttributes() {
    return {
      ...this.parent?.(),
      tight: {
        default: true,
        parseHTML: (el: HTMLElement) => el.getAttribute("data-tight") === "true" || !el.querySelector("p"),
        renderHTML: (attrs: { tight?: boolean }) => (attrs.tight ? { "data-tight": "true" } : {}),
      },
    };
  },
});

type MdState = {
  write: (s: string) => void;
  ensureNewLine: () => void;
  closeBlock: (n: PmNode) => void;
  renderInline: (n: PmNode) => void;
  text: (s: string, escape?: boolean) => void;
  inTable?: boolean;
};

const MarkdownTable = Table.extend({
  addStorage() {
    return {
      markdown: {
        serialize(state: MdState, node: PmNode) {
          state.inTable = true;
          node.forEach((row, _p, i) => {
            state.write("| ");
            row.forEach((cell, _p2, j) => {
              if (j) state.write(" | ");
              cell.forEach((block, _p3, k) => {
                if (k) state.write(" ");
                if (block.isTextblock) state.renderInline(block);
                else state.text(block.textContent.replace(/\s+/g, " ").trim());
              });
            });
            state.write(" |");
            state.ensureNewLine();
            if (!i) {
              state.write(`| ${Array.from({ length: row.childCount }, () => "---").join(" | ")} |`);
              state.ensureNewLine();
            }
          });
          state.closeBlock(node);
          state.inTable = false;
        },
        parse: {},
      },
    };
  },
});

const WikiRefs = Extension.create({
  name: "wikiRefs",
  addProseMirrorPlugins() {
    const find = (doc: PmNode) => {
      const marks: Decoration[] = [];
      doc.descendants((node, pos) => {
        if (!node.isText || !node.text) return;
        for (const m of node.text.matchAll(WIKI)) {
          const from = pos + (m.index ?? 0);
          marks.push(Decoration.inline(from, from + m[0].length, { class: "wiki-ref" }));
        }
      });
      return DecorationSet.create(doc, marks);
    };
    return [
      new Plugin({
        key: new PluginKey("wikiRefs"),
        state: {
          init: (_, { doc }) => find(doc),
          apply: (tr, old) => (tr.docChanged ? find(tr.doc) : old),
        },
        props: {
          decorations(state) {
            return this.getState(state);
          },
        },
      }),
    ];
  },
});

function extensions(placeholder: string) {
  return [
    StarterKit.configure({
      text: false,
      underline: false,
      heading: { levels: [1, 2, 3, 4] },
      link: { openOnClick: false, autolink: true, linkOnPaste: true, HTMLAttributes: { rel: "noopener noreferrer", target: null } },
    }),
    MarkdownText,
    Image.configure({ inline: false, allowBase64: false }),
    TightTaskList,
    TaskItem.configure({ nested: true }),
    MarkdownTable.configure({ resizable: false }),
    TableRow,
    TableHeader,
    TableCell,
    WikiRefs,
    Placeholder.configure({
      showOnlyCurrent: true,
      includeChildren: true,
      placeholder: ({ node }) => {
        if (node.type.name === "heading") return `Heading ${node.attrs.level}`;
        if (node.type.name === "paragraph") return placeholder;
        return "";
      },
    }),
    Markdown.configure({ html: false, tightLists: true, bulletListMarker: "-", linkify: false, breaks: false, transformPastedText: true, transformCopiedText: false }),
  ];
}

function markdownOf(editor: Editor): string {
  return ((editor.storage as unknown as { markdown: { getMarkdown: () => string } }).markdown.getMarkdown() ?? "").replace(/\s+$/, "") + "\n";
}

function skeleton(text: string): string {
  return text
    .replace(/\]\([^)]*\)/g, "")
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, "");
}

export function keepsEverything(original: string, roundTrip: string): boolean {
  if (/^\[[a-zA-Z]+\]$/m.test(roundTrip) && !/^\[[a-zA-Z]+\]$/m.test(original)) return false;
  const strip = (s: string) => skeleton(s.replace(/<!--[\s\S]*?-->/g, ""));
  return strip(original) === strip(roundTrip) && !/<!--/.test(original);
}

type Command = { id: string; label: string; hint: string; icon: PhosphorIcon; keys: string[]; run: (e: Editor) => void };

const COMMANDS: Command[] = [
  { id: "text", label: "Text", hint: "Plain paragraph", icon: TextT, keys: ["text", "paragraph", "p"], run: (e) => e.chain().focus().setParagraph().run() },
  { id: "h1", label: "Heading 1", hint: "Big section title", icon: TextHOne, keys: ["h1", "heading", "title"], run: (e) => e.chain().focus().setHeading({ level: 1 }).run() },
  { id: "h2", label: "Heading 2", hint: "Medium section title", icon: TextHTwo, keys: ["h2", "heading", "subtitle"], run: (e) => e.chain().focus().setHeading({ level: 2 }).run() },
  { id: "h3", label: "Heading 3", hint: "Small section title", icon: TextHThree, keys: ["h3", "heading"], run: (e) => e.chain().focus().setHeading({ level: 3 }).run() },
  { id: "bullet", label: "Bulleted list", hint: "A simple list", icon: ListBullets, keys: ["bullet", "list", "ul", "unordered"], run: (e) => e.chain().focus().toggleBulletList().run() },
  { id: "number", label: "Numbered list", hint: "A list with numbers", icon: ListNumbers, keys: ["number", "ordered", "ol", "list"], run: (e) => e.chain().focus().toggleOrderedList().run() },
  { id: "todo", label: "To-do list", hint: "Track things with checkboxes", icon: CheckSquare, keys: ["todo", "task", "check", "checkbox"], run: (e) => e.chain().focus().toggleTaskList().run() },
  { id: "quote", label: "Quote", hint: "Call out a passage", icon: Quotes, keys: ["quote", "blockquote", "callout"], run: (e) => e.chain().focus().toggleBlockquote().run() },
  { id: "code", label: "Code", hint: "A block of code", icon: CodeBlock, keys: ["code", "snippet", "pre"], run: (e) => e.chain().focus().toggleCodeBlock().run() },
  { id: "table", label: "Table", hint: "Rows and columns", icon: TableIcon, keys: ["table", "grid"], run: (e) => e.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run() },
  { id: "divider", label: "Divider", hint: "A line between sections", icon: Minus, keys: ["divider", "hr", "line", "rule", "separator"], run: (e) => e.chain().focus().setHorizontalRule().run() },
  { id: "report", label: "Link to report", hint: "Type a report title", icon: FileText, keys: ["link", "report", "wiki", "page"], run: (e) => e.chain().focus().insertContent("[[").run() },
];

type Popup = { kind: "slash" | "wiki"; from: number; query: string; top: number; left: number };
type Item = { key: string; label: string; hint?: string; icon: PhosphorIcon; apply: (e: Editor, p: Popup) => void };

function popupAt(editor: Editor): Popup | null {
  const { state, view } = editor;
  const sel = state.selection;
  if (!sel.empty || !editor.isFocused) return null;
  const $from = sel.$from;
  if ($from.parent.type.spec.code) return null;
  const before = $from.parent.textBetween(0, $from.parentOffset, undefined, "￼");
  const wiki = before.match(/\[\[([^\]\n[]{0,60})$/);
  const slash = !wiki && before.match(/(?:^|\s)\/([a-z0-9 -]{0,24})$/i);
  const match = wiki || slash;
  if (!match) return null;
  const typed = match[0].length - (wiki ? 0 : match[0].startsWith("/") ? 0 : 1);
  const from = $from.pos - typed;
  const at = view.coordsAtPos($from.pos);
  return { kind: wiki ? "wiki" : "slash", from, query: match[1], top: at.bottom + 6, left: at.left };
}

function rank(label: string, keys: string[], query: string): number {
  const q = query.trim().toLowerCase();
  if (!q) return 1;
  const l = label.toLowerCase();
  if (l.startsWith(q)) return 3;
  if (keys.some((k) => k.startsWith(q))) return 2;
  return l.includes(q) ? 1 : 0;
}

function findText(doc: PmNode, point: FocusPoint): number | null {
  const tries = [point.before + point.after, point.before, point.after].filter((t) => (t ?? "").trim().length >= 3);
  for (const needle of tries) {
    let found: number | null = null;
    doc.descendants((node, pos) => {
      if (found !== null) return false;
      if (!node.isText || !node.text) return;
      const at = node.text.indexOf(needle);
      if (at >= 0) found = pos + at + (needle === point.after ? 0 : point.before.length);
    });
    if (found !== null) return found;
  }
  return null;
}

export interface DocEditorHandle {
  focus: (at?: "start" | "end") => void;
  markdown: () => string;
}

export const DocEditor = forwardRef<
  DocEditorHandle,
  {
    value: string;
    onChange: (markdown: string) => void;
    onLossy?: () => void;
    onSave?: () => void;
    placeholder?: string;
    titles?: string[];
    autoFocus?: boolean;
    focusPoint?: FocusPoint | null;
    className?: string;
  }
>(function DocEditor({ value, onChange, onLossy, onSave, placeholder = "Write, or press / for blocks", titles = [], autoFocus, focusPoint, className }, ref) {
  const [popup, setPopup] = useState<Popup | null>(null);
  const [index, setIndex] = useState(0);
  const [linking, setLinking] = useState<string | null>(null);
  const changeRef = useRef(onChange);
  changeRef.current = onChange;
  const saveRef = useRef(onSave);
  saveRef.current = onSave;
  const lossyRef = useRef(onLossy);
  lossyRef.current = onLossy;
  const popupRef = useRef<Popup | null>(null);
  popupRef.current = popup;
  const itemsRef = useRef<Item[]>([]);
  const indexRef = useRef(0);
  indexRef.current = index;
  const lastRef = useRef(value);
  const dismissedRef = useRef<number | null>(null);
  const show = (ed: Editor) => {
    const next = popupAt(ed);
    if (next && next.from === dismissedRef.current) return setPopup(null);
    if (!next) dismissedRef.current = null;
    setPopup(next);
  };

  const editor = useEditor({
    immediatelyRender: false,
    extensions: useMemo(() => extensions(placeholder), [placeholder]),
    content: value,
    autofocus: false,
    editorProps: {
      attributes: { spellcheck: "true", "aria-label": "Report body", role: "textbox", "aria-multiline": "true" },
      handleKeyDown: (view, event) => {
        const mod = event.metaKey || event.ctrlKey;
        if (mod && event.key.toLowerCase() === "s") {
          event.preventDefault();
          saveRef.current?.();
          return true;
        }
        if (mod && event.key.toLowerCase() === "k") {
          event.preventDefault();
          event.stopPropagation();
          if (!view.state.selection.empty) setLinking("");
          return true;
        }
        const open = popupRef.current;
        const items = itemsRef.current;
        if (!open || !items.length) return false;
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault();
          const step = event.key === "ArrowDown" ? 1 : -1;
          setIndex((i) => (i + step + items.length) % items.length);
          return true;
        }
        if (event.key === "Enter" || event.key === "Tab") {
          event.preventDefault();
          const item = items[Math.min(indexRef.current, items.length - 1)];
          if (item && editorRef.current) item.apply(editorRef.current, open);
          setPopup(null);
          return true;
        }
        if (event.key === "Escape") {
          takeEscape(event);
          event.stopPropagation();
          dismissedRef.current = open.from;
          setPopup(null);
          return true;
        }
        return false;
      },
    },
    onCreate: ({ editor: ed }) => {
      if (!keepsEverything(value, markdownOf(ed))) lossyRef.current?.();
      if (!autoFocus && !focusPoint) return;
      window.requestAnimationFrame(() => {
        if (ed.isDestroyed) return;
        const pos = focusPoint ? findText(ed.state.doc, focusPoint) ?? ed.view.posAtCoords({ left: focusPoint.x, top: focusPoint.y })?.pos ?? null : null;
        if (focusPoint && pos !== null) {
          let box = ed.view.dom.parentElement;
          while (box && !/(auto|scroll)/.test(getComputedStyle(box).overflowY)) box = box.parentElement;
          if (box) box.scrollTop += ed.view.coordsAtPos(pos).top - focusPoint.y + 8;
        }
        if (pos !== null) ed.chain().setTextSelection(pos).focus(undefined, { scrollIntoView: false }).run();
        else ed.commands.focus(value.trim() ? "start" : "end", { scrollIntoView: false });
      });
    },
    onUpdate: ({ editor: ed }) => {
      const md = markdownOf(ed);
      if (/^\[[a-zA-Z]+\]$/m.test(md) && !/^\[[a-zA-Z]+\]$/m.test(lastRef.current)) {
        lossyRef.current?.();
        return;
      }
      lastRef.current = md;
      changeRef.current(md);
      show(ed);
    },
    onSelectionUpdate: ({ editor: ed }) => show(ed),
    onBlur: () => window.setTimeout(() => setPopup(null), 120),
  });
  const editorRef = useRef<Editor | null>(null);
  editorRef.current = editor;

  useEffect(() => {
    if (!editor || value === lastRef.current) return;
    lastRef.current = value;
    editor.commands.setContent(value, { emitUpdate: false });
  }, [value, editor]);

  useImperativeHandle(
    ref,
    () => ({
      focus: (at = "end") => editor?.commands.focus(at),
      markdown: () => (editor ? markdownOf(editor) : value),
    }),
    [editor, value],
  );

  const items = useMemo<Item[]>(() => {
    if (!popup) return [];
    if (popup.kind === "wiki") {
      const q = popup.query.trim().toLowerCase();
      const found = titles
        .filter((t) => !q || t.toLowerCase().includes(q))
        .sort((a, b) => Number(b.toLowerCase().startsWith(q)) - Number(a.toLowerCase().startsWith(q)) || a.localeCompare(b))
        .slice(0, 8);
      const exact = titles.some((t) => t.toLowerCase() === q);
      const list = found.map<Item>((t) => ({ key: `r:${t}`, label: t, icon: FileText, apply: (e, p) => insertWiki(e, p, t) }));
      if (q && !exact) list.push({ key: "new", label: popup.query.trim(), hint: "New report link", icon: FileText, apply: (e, p) => insertWiki(e, p, popup.query.trim()) });
      return list;
    }
    return COMMANDS.map((c) => ({ c, score: rank(c.label, c.keys, popup.query) }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score)
      .map<Item>(({ c }) => ({
        key: c.id,
        label: c.label,
        hint: c.hint,
        icon: c.icon,
        apply: (e, p) => {
          e.chain().focus().deleteRange({ from: p.from, to: e.state.selection.from }).run();
          c.run(e);
        },
      }));
  }, [popup, titles]);
  itemsRef.current = items;

  useEffect(() => setIndex(0), [popup?.kind, popup?.query]);

  const setLink = useCallback(
    (href: string) => {
      if (!editor) return;
      const url = href.trim();
      if (!url) editor.chain().focus().extendMarkRange("link").unsetLink().run();
      else editor.chain().focus().extendMarkRange("link").setLink({ href: /^[a-z][a-z0-9+.-]*:|^[/#]/i.test(url) ? url : `https://${url}` }).run();
      setLinking(null);
    },
    [editor],
  );

  if (!editor) return <div className={cn(DOC_PROSE, "min-h-[40vh]", className)} />;

  return (
    <div className={cn(DOC_PROSE, className)}>
      <BubbleMenu
        editor={editor}
        options={{ placement: "top", offset: 8 }}
        shouldShow={({ editor: ed, state }) => !state.selection.empty && ed.isEditable && !ed.isActive("codeBlock") && !ed.isActive("image") && state.selection.constructor.name !== "CellSelection"}
        className={cn(MENU, "flex items-center gap-0.5 p-1 shadow-[0_3px_0_rgb(var(--w-ink)),0_12px_28px_-12px_rgb(0_0_0/0.45)]")}
      >
        {linking !== null ? (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setLink(linking);
            }}
            className="flex items-center gap-1"
          >
            <input
              autoFocus
              value={linking}
              onChange={(e) => setLinking(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  takeEscape(e);
                  e.stopPropagation();
                  setLinking(null);
                  editor.commands.focus();
                }
              }}
              placeholder="Paste a link, Enter to apply"
              aria-label="Link address"
              className="h-7 w-60 rounded-md bg-w-field px-2 text-w-sm outline-none ring-1 ring-inset ring-w-line focus:ring-w-teal"
            />
          </form>
        ) : (
          <>
            <BubbleButton label="Text" active={editor.isActive("paragraph")} onClick={() => editor.chain().focus().setParagraph().run()}>
              <TextT size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Heading 1" active={editor.isActive("heading", { level: 1 })} onClick={() => editor.chain().focus().toggleHeading({ level: 1 }).run()}>
              <TextHOne size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Heading 2" active={editor.isActive("heading", { level: 2 })} onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}>
              <TextHTwo size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Heading 3" active={editor.isActive("heading", { level: 3 })} onClick={() => editor.chain().focus().toggleHeading({ level: 3 }).run()}>
              <TextHThree size={15} weight="bold" />
            </BubbleButton>
            <span className="mx-0.5 h-4 w-px bg-w-line2" aria-hidden />
            <BubbleButton label="Bold" keys="⌘B" active={editor.isActive("bold")} onClick={() => editor.chain().focus().toggleBold().run()}>
              <TextB size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Italic" keys="⌘I" active={editor.isActive("italic")} onClick={() => editor.chain().focus().toggleItalic().run()}>
              <TextItalic size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Strikethrough" active={editor.isActive("strike")} onClick={() => editor.chain().focus().toggleStrike().run()}>
              <TextStrikethrough size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Inline code" keys="⌘E" active={editor.isActive("code")} onClick={() => editor.chain().focus().toggleCode().run()}>
              <Code size={15} weight="bold" />
            </BubbleButton>
            <BubbleButton label="Link" keys="⌘K" active={editor.isActive("link")} onClick={() => setLinking((editor.getAttributes("link").href as string | undefined) ?? "")}>
              <LinkIcon size={15} weight="bold" />
            </BubbleButton>
          </>
        )}
      </BubbleMenu>
      <EditorContent editor={editor} />
      {popup && items.length > 0 && <PopupMenu popup={popup} items={items} index={index} onHover={setIndex} onPick={(item) => {
        item.apply(editor, popup);
        setPopup(null);
      }} />}
    </div>
  );
});

function insertWiki(editor: Editor, popup: Popup, title: string) {
  const to = editor.state.selection.from;
  const after = editor.state.doc.textBetween(to, Math.min(to + 2, editor.state.doc.content.size), undefined, "￼");
  editor
    .chain()
    .focus()
    .deleteRange({ from: popup.from, to: after === "]]" ? to + 2 : to })
    .insertContent(`[[${title}]] `)
    .run();
}

function BubbleButton({ label, keys, active, onClick, children }: { label: string; keys?: string; active?: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      title={keys ? `${label} (${keys})` : label}
      aria-label={label}
      aria-pressed={active}
      onMouseDown={(e) => {
        e.preventDefault();
        onClick();
      }}
      className={cn("grid h-7 w-7 place-items-center rounded-md transition-colors", active ? "bg-w-invert text-w-oninvert" : "text-w-text2 hover:bg-w-sand hover:text-w-strong")}
    >
      {children}
    </button>
  );
}

function PopupMenu({ popup, items, index, onHover, onPick }: { popup: Popup; items: Item[]; index: number; onHover: (i: number) => void; onPick: (item: Item) => void }) {
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-index="${index}"]`);
    if (el && listRef.current) {
      const box = listRef.current;
      if (el.offsetTop < box.scrollTop) box.scrollTop = el.offsetTop - 4;
      else if (el.offsetTop + el.offsetHeight > box.scrollTop + box.clientHeight) box.scrollTop = el.offsetTop + el.offsetHeight - box.clientHeight + 4;
    }
  }, [index]);
  const height = 320;
  const below = popup.top + height < window.innerHeight - 12;
  const style = {
    left: Math.max(8, Math.min(popup.left, window.innerWidth - 300)),
    ...(below ? { top: popup.top } : { bottom: window.innerHeight - popup.top + 30 }),
  };
  return createPortal(
    <div
      ref={listRef}
      role="listbox"
      aria-label={popup.kind === "wiki" ? "Reports" : "Blocks"}
      style={style}
      className={cn(MENU, "fixed z-[80] max-h-80 w-72 overflow-y-auto")}
      onMouseDown={(e) => e.preventDefault()}
    >
      <p className="px-2 pb-1 pt-0.5 text-w-2xs font-medium text-w-muted">{popup.kind === "wiki" ? "Link a report" : "Blocks"}</p>
      {items.map((item, i) => {
        const ItemIcon = item.icon;
        return (
          <button
            key={item.key}
            type="button"
            role="option"
            aria-selected={i === index}
            data-index={i}
            onMouseEnter={() => onHover(i)}
            onClick={() => onPick(item)}
            className={cn("flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left transition-colors duration-100", i === index ? "bg-w-sand text-w-strong" : "text-w-text2")}
          >
            <span className="grid h-8 w-8 shrink-0 place-items-center rounded-md border-2 border-w-line bg-w-card">
              <ItemIcon size={16} weight="duotone" />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-w-sm font-medium">{item.label}</span>
              {item.hint && <span className="block truncate text-w-2xs text-w-muted">{item.hint}</span>}
            </span>
          </button>
        );
      })}
    </div>,
    document.body,
  );
}
