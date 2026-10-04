"use client";

import { createContext, useContext, useMemo, useRef, useState } from "react";
import { createStateStore, defineRegistry, JSONUIProvider, Renderer, useBoundProp, type StateStore } from "@json-render/react";
import { ArrowDownRight, ArrowRight, ArrowUpRight } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { checkUiSpec, uiCatalog } from "@/lib/ui-catalog";
import type { WorldAgent } from "@/lib/world";
import { RichText } from "./chat-kit";
import { EmbedView, embedFromHref } from "./embeds";
import { CHUNKY, INPUT } from "./styles";
import { WSelect } from "./controls";
import { ChartView } from "./ui-chart";

type Ctx = {
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  locked: boolean;
};

const UiContext = createContext<Ctx>({ handles: new Map(), onOpenAgent: () => {}, locked: true });

const TONE: Record<string, string> = {
  normal: "",
  muted: "text-w-muted",
  ok: "text-w-ok",
  warn: "text-w-warn",
  danger: "text-w-danger",
};

const BADGE_TONE: Record<string, string> = {
  normal: "bg-w-field",
  muted: "bg-w-field text-w-muted",
  ok: "bg-w-ok/20",
  warn: "bg-w-lemon",
  danger: "bg-w-alert text-w-onalert",
};

const GAP: Record<string, string> = { sm: "gap-1.5", md: "gap-3", lg: "gap-5" };

const BUTTON: Record<string, string> = {
  primary: "bg-w-teal hover:bg-w-teal2",
  secondary: "bg-w-card hover:bg-w-field",
  danger: "bg-w-alert text-w-onalert",
};

function Md({ text, className }: { text: string; className?: string }) {
  const { handles, onOpenAgent } = useContext(UiContext);
  return <RichText text={text} handles={handles} onOpenAgent={onOpenAgent} className={className} />;
}

function cell(value: string | number | null | undefined): string {
  return value === null || value === undefined ? "" : String(value);
}

function safeHref(href: string): string | null {
  try {
    const url = new URL(href);
    return url.protocol === "https:" || url.protocol === "http:" ? url.toString() : null;
  } catch {
    return null;
  }
}

const { registry } = defineRegistry(uiCatalog, {
  components: {
    Card: ({ props, children }) => (
      <div className="rounded-lg border-2 border-w-line2 bg-w-field/50 p-3">
        {props.title && <p className="text-w-md font-semibold leading-tight text-w-strong">{props.title}</p>}
        {props.subtitle && <p className="text-w-xs text-w-muted">{props.subtitle}</p>}
        <div className={cn("flex flex-col gap-2", (props.title || props.subtitle) && "mt-2")}>{children}</div>
      </div>
    ),
    Stack: ({ props, children }) => (
      <div className={cn("flex", props.direction === "row" ? "flex-row flex-wrap items-stretch" : "flex-col", GAP[props.gap ?? "md"])}>{children}</div>
    ),
    Heading: ({ props }) => <p className="text-w-md font-semibold text-w-strong">{props.text}</p>,
    Text: ({ props }) => <Md text={props.text} className={TONE[props.tone ?? "normal"]} />,
    Metric: ({ props }) => (
      <div className="min-w-[7rem] flex-1 rounded-lg border-2 border-w-line bg-w-card px-3 py-2">
        <p className="text-w-xs font-semibold text-w-muted">{props.label}</p>
        <p className="flex items-center gap-1 text-w-xl font-bold leading-tight">
          {cell(props.value)}
          {props.trend === "up" && <ArrowUpRight size={16} weight="bold" className="text-w-ok" />}
          {props.trend === "down" && <ArrowDownRight size={16} weight="bold" className="text-w-danger" />}
          {props.trend === "flat" && <ArrowRight size={16} weight="bold" className="text-w-muted" />}
        </p>
        {props.hint && <p className="text-w-xs text-w-text2">{props.hint}</p>}
      </div>
    ),
    Progress: ({ props }) => (
      <div>
        {props.label && (
          <p className="mb-1 flex justify-between text-w-xs">
            <span>{props.label}</span>
            <span className="text-w-muted">{Math.round(props.value)}%</span>
          </p>
        )}
        <div className="h-2.5 overflow-hidden rounded-[0.1875rem] border-2 border-w-ink bg-w-field" role="progressbar" aria-valuenow={props.value} aria-valuemin={0} aria-valuemax={100} aria-label={props.label ?? "Progress"}>
          <div className="h-full bg-w-teal" style={{ width: `${Math.max(0, Math.min(100, props.value))}%` }} />
        </div>
      </div>
    ),
    Badge: ({ props }) => (
      <span className={cn("inline-block w-fit rounded-md border-2 border-w-ink px-1.5 text-w-xs font-semibold", BADGE_TONE[props.tone ?? "normal"])}>{props.label}</span>
    ),
    Table: ({ props }) => (
      <div className="overflow-x-auto rounded-lg border-2 border-w-line">
        <table className="w-full text-left text-w-sm">
          <thead className="bg-w-field">
            <tr>
              {props.columns.map((c, i) => (
                <th key={i} className="px-2 py-1 font-semibold">{c}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {props.rows.map((row, r) => (
              <tr key={r} className="border-t border-w-line">
                {props.columns.map((_, i) => (
                  <td key={i} className="px-2 py-1 align-top">{cell(row[i])}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    ),
    Chart: ({ props }) => <ChartView kind={props.kind} title={props.title} labels={props.labels} series={props.series} unit={props.unit} stacked={props.stacked} />,
    List: ({ props }) => {
      const Tag = props.ordered ? "ol" : "ul";
      return (
        <Tag className={cn("space-y-0.5 pl-5", props.ordered ? "list-decimal" : "list-disc")}>
          {props.items.map((item, i) => (
            <li key={i}>
              <Md text={item} />
            </li>
          ))}
        </Tag>
      );
    },
    Divider: () => <hr className="border-t-2 border-dashed border-w-line2" />,
    Ref: ({ props }) => {
      const embed = embedFromHref(props.ref.trim());
      return embed ? <EmbedView embed={embed} card /> : <span className="font-mono text-w-xs text-w-muted">{props.ref}</span>;
    },
    Link: ({ props }) => {
      const href = safeHref(props.href);
      return href ? (
        <a href={href} target="_blank" rel="noopener noreferrer" className="w-fit text-w-link underline">
          {props.label}
        </a>
      ) : (
        <span>{props.label}</span>
      );
    },
    Button: ({ props, emit }) => {
      const { locked } = useContext(UiContext);
      return (
        <button type="button" disabled={locked} onClick={() => emit("press")} className={cn(CHUNKY, "w-fit px-3 py-1 text-w-sm", BUTTON[props.variant ?? "primary"])}>
          {props.label}
        </button>
      );
    },
    TextInput: ({ props, bindings }) => {
      const { locked } = useContext(UiContext);
      const [value, setValue] = useBoundProp<string>(typeof props.value === "string" ? props.value : undefined, bindings?.value);
      const shared = {
        "aria-label": props.label,
        placeholder: props.placeholder ?? "",
        value: value ?? "",
        disabled: locked,
        maxLength: 4000,
      };
      return (
        <label className="flex flex-col gap-1 text-w-xs font-semibold">
          {props.label}
          {props.multiline ? (
            <textarea {...shared} rows={3} onChange={(e) => setValue(e.target.value)} className={cn(INPUT, "resize-y font-normal")} />
          ) : (
            <input {...shared} onChange={(e) => setValue(e.target.value)} className={cn(INPUT, "font-normal")} />
          )}
        </label>
      );
    },
    Select: ({ props, bindings }) => {
      const { locked } = useContext(UiContext);
      const [value, setValue] = useBoundProp<string>(typeof props.value === "string" ? props.value : undefined, bindings?.value);
      return (
        <label className="flex flex-col gap-1 text-w-xs font-semibold">
          {props.label}
          <WSelect ariaLabel={props.label} disabled={locked} value={value ?? ""} onChange={setValue} placeholder="Choose" className="w-full font-normal" options={props.options.map((o) => ({ value: o, label: o }))} />
        </label>
      );
    },
    Checkbox: ({ props, bindings }) => {
      const { locked } = useContext(UiContext);
      const [checked, setChecked] = useBoundProp<boolean>(typeof props.checked === "boolean" ? props.checked : undefined, bindings?.checked);
      return (
        <label className="flex w-fit items-center gap-2 text-w-sm">
          <input type="checkbox" className="h-4 w-4 accent-w-teal" disabled={locked} checked={!!checked} onChange={(e) => setChecked(e.target.checked)} />
          {props.label}
        </label>
      );
    },
  },
  actions: {
    reply: async () => {},
    submit: async () => {},
  },
});

function flatten(value: unknown, path: string, out: string[]) {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    for (const [k, v] of Object.entries(value)) flatten(v, path ? `${path}.${k}` : k, out);
    return;
  }
  const text = Array.isArray(value) ? value.map((v) => (typeof v === "object" ? JSON.stringify(v) : String(v))).join(", ") : value === undefined || value === null ? "" : String(value);
  out.push(`- ${path || "value"}: ${text || "(empty)"}`);
}

export function formAnswer(state: Record<string, unknown>, label?: string | null): string {
  const lines: string[] = [];
  flatten(state, "", lines);
  return `${label?.trim() || "My answers"}:\n${lines.join("\n")}`;
}

type KeptCard = { sig: string; store: StateStore; sent: boolean };

const KEPT_CARDS = new Map<string, KeptCard>();
const KEPT_LIMIT = 200;

function pickedState(seed: unknown, answer: string | undefined): Record<string, unknown> {
  const state = structuredClone((seed as Record<string, unknown> | undefined) ?? {});
  for (const line of answer?.split("\n") ?? []) {
    const m = line.match(/^- ([^:]+): (.*)$/);
    if (!m) continue;
    const path = m[1].split(".");
    const name = path.pop() as string;
    let parent: unknown = state;
    for (const part of path) parent = parent && typeof parent === "object" && !Array.isArray(parent) ? (parent as Record<string, unknown>)[part] : undefined;
    if (!parent || typeof parent !== "object" || Array.isArray(parent) || !(name in parent)) continue;
    const fields = parent as Record<string, unknown>;
    const was = fields[name];
    const text = m[2] === "(empty)" ? "" : m[2];
    if (typeof was === "boolean") fields[name] = text === "true";
    else if (typeof was === "number") fields[name] = text !== "" && Number.isFinite(Number(text)) ? Number(text) : was;
    else if (typeof was === "string" || was === null) fields[name] = text;
    else if (Array.isArray(was) && was.every((v) => typeof v === "string")) fields[name] = text ? text.split(", ") : [];
  }
  return state;
}

function keptCard(key: string, sig: string, seed: unknown, answer: string | undefined): KeptCard {
  const kept = KEPT_CARDS.get(key);
  KEPT_CARDS.delete(key);
  const card = kept && kept.sig === sig ? kept : { sig, store: createStateStore(pickedState(seed, answer)), sent: false };
  KEPT_CARDS.set(key, card);
  if (KEPT_CARDS.size > KEPT_LIMIT) {
    const oldest = KEPT_CARDS.keys().next().value;
    if (oldest !== undefined) KEPT_CARDS.delete(oldest);
  }
  return card;
}

function signature(raw: unknown): string {
  try {
    return JSON.stringify(raw ?? null);
  } catch {
    return "null";
  }
}

export function UiCard({
  spec: raw,
  stateKey,
  handles,
  onOpenAgent,
  canAnswer,
  answered,
  answeredLabel = "Answered",
  answerText,
  onAnswer,
}: {
  spec: unknown;
  stateKey: string;
  handles: Map<string, WorldAgent>;
  onOpenAgent: (id: string) => void;
  canAnswer: boolean;
  answered: boolean;
  answeredLabel?: string;
  answerText?: string;
  onAnswer: (text: string) => Promise<boolean>;
}) {
  const sig = useMemo(() => signature(raw), [raw]);
  const spec = useMemo(() => checkUiSpec(JSON.parse(sig)), [sig]);
  const restore = answered ? answerText : undefined;
  const card = useMemo(() => keptCard(stateKey, sig, spec?.state, restore), [stateKey, sig, spec, restore]);
  const cardRef = useRef(card);
  cardRef.current = card;
  const [sending, setSending] = useState(false);
  const answer = useRef(onAnswer);
  answer.current = onAnswer;
  const locked = !canAnswer || answered || sending || card.sent;
  const lockedRef = useRef(locked);
  lockedRef.current = locked;

  const handlers = useMemo(() => {
    const send = async (text: string) => {
      const body = text.trim();
      if (!body || lockedRef.current) return;
      const sentFrom = cardRef.current;
      lockedRef.current = true;
      setSending(true);
      const ok = await answer.current(body.slice(0, 8000));
      if (ok) sentFrom.sent = true;
      setSending(false);
    };
    return {
      reply: (params: Record<string, unknown>) => send(typeof params.text === "string" ? params.text : ""),
      submit: (params: Record<string, unknown>) => send(formAnswer(cardRef.current.store.getSnapshot(), typeof params.label === "string" ? params.label : null)),
    };
  }, []);

  const ctx = useMemo(() => ({ handles, onOpenAgent, locked }), [handles, onOpenAgent, locked]);

  if (!spec) return <p className="mt-2 text-w-xs text-w-muted">This card could not be shown here.</p>;
  return (
    <UiContext.Provider value={ctx}>
      <div className="mt-2 flex flex-col gap-2">
        <JSONUIProvider registry={registry} store={card.store} handlers={handlers}>
          <Renderer spec={spec} registry={registry} />
        </JSONUIProvider>
        {(answered || card.sent) && <p className="text-w-xs font-semibold text-w-muted">{answeredLabel}</p>}
      </div>
    </UiContext.Provider>
  );
}
