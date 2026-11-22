import { defineCatalog, validateSpec, type Spec } from "@json-render/core";
import { schema } from "@json-render/react/schema";
import { z } from "zod";

const tone = z.enum(["normal", "muted", "ok", "warn", "danger"]).nullish();
const bound = <T extends z.ZodTypeAny>(inner: T) => z.union([inner, z.record(z.string(), z.unknown())]);

export const uiCatalog = defineCatalog(schema, {
  components: {
    Card: {
      props: z.object({ title: z.string().nullish(), subtitle: z.string().nullish() }),
      slots: ["default"],
      description: "A bordered box that groups children, with an optional title and subtitle",
    },
    Stack: {
      props: z.object({ direction: z.enum(["row", "column"]).nullish(), gap: z.enum(["sm", "md", "lg"]).nullish() }),
      slots: ["default"],
      description: "Lays children out in a row (wraps) or a column",
    },
    Heading: {
      props: z.object({ text: z.string() }),
      description: "A short section heading",
    },
    Text: {
      props: z.object({ text: z.string(), tone }),
      description: "A paragraph of markdown text",
    },
    Metric: {
      props: z.object({ label: z.string(), value: z.union([z.string(), z.number()]), hint: z.string().nullish(), trend: z.enum(["up", "down", "flat"]).nullish() }),
      description: "One big number or value with a label",
    },
    Progress: {
      props: z.object({ label: z.string().nullish(), value: z.number().min(0).max(100) }),
      description: "A progress bar from 0 to 100",
    },
    Badge: {
      props: z.object({ label: z.string(), tone }),
      description: "A small coloured label",
    },
    Table: {
      props: z.object({ columns: z.array(z.string()).max(8), rows: z.array(z.array(z.union([z.string(), z.number(), z.null()])).max(8)).max(50) }),
      description: "A table; every row has one cell per column",
    },
    Chart: {
      props: z.object({
        kind: z.enum(["bar", "line", "area", "pie"]).nullish(),
        title: z.string().nullish(),
        labels: z.array(z.string()).min(1).max(40),
        series: z.array(z.object({ name: z.string(), values: z.array(z.number()).min(1).max(40) })).min(1).max(5),
        unit: z.string().nullish(),
        stacked: z.boolean().nullish(),
      }),
      description: "A bar, line, area or pie chart; every series has one number per label",
    },
    List: {
      props: z.object({ items: z.array(z.string()).max(30), ordered: z.boolean().nullish() }),
      description: "A bulleted or numbered list of markdown lines",
    },
    Divider: {
      props: z.object({}),
      description: "A horizontal rule",
    },
    Ref: {
      props: z.object({ ref: z.string() }),
      description: "A live Loka card for quest:<id>, report:<id>, todo:<id>, msg:<id>, challenge:<id> or course:<id>",
    },
    Link: {
      props: z.object({ label: z.string(), href: z.string() }),
      description: "An https link that opens in a new tab",
    },
    Button: {
      props: z.object({ label: z.string(), variant: z.enum(["primary", "secondary", "danger"]).nullish() }),
      description: "A button; bind on.press to an action",
    },
    TextInput: {
      props: z.object({ label: z.string(), value: bound(z.string()).nullish(), placeholder: z.string().nullish(), multiline: z.boolean().nullish() }),
      description: "A text field; bind value with {\"$bindState\": \"/path\"}",
    },
    Select: {
      props: z.object({ label: z.string(), value: bound(z.string()).nullish(), options: z.array(z.string()).max(20) }),
      description: "A dropdown; bind value with {\"$bindState\": \"/path\"}",
    },
    Checkbox: {
      props: z.object({ label: z.string(), checked: bound(z.boolean()).nullish() }),
      description: "A checkbox; bind checked with {\"$bindState\": \"/path\"}",
    },
  },
  actions: {
    reply: {
      params: z.object({ text: z.string() }),
      description: "Send text to the agent as the person's answer",
    },
    submit: {
      params: z.object({ label: z.string().nullish() }),
      description: "Send every value in state to the agent as the person's answer",
    },
  },
});

export const UI_LIMITS = { bytes: 32768, elements: 80 };

const ACTIONS = new Set(["reply", "submit", "setState", "pushState", "removeState", "validateForm"]);

function bindings(on: unknown): { action?: unknown }[] {
  if (!on || typeof on !== "object") return [];
  return Object.values(on as Record<string, unknown>).flatMap((b) => (Array.isArray(b) ? b : [b])) as { action?: unknown }[];
}

export function checkUiSpec(raw: unknown): Spec | null {
  if (!raw || typeof raw !== "object") return null;
  if (JSON.stringify(raw).length > UI_LIMITS.bytes) return null;
  const given = raw as Spec;
  if (typeof given.root !== "string" || !given.elements || typeof given.elements !== "object" || Array.isArray(given.elements)) return null;
  const filled: Spec["elements"] = {};
  for (const [key, el] of Object.entries(given.elements)) {
    if (!el || typeof el !== "object") return null;
    filled[key] = { ...el, props: el.props ?? {}, children: el.children ?? [] };
  }
  const spec: Spec = { ...given, elements: filled };
  const elements = Object.values(spec.elements);
  if (!elements.length || elements.length > UI_LIMITS.elements || !spec.elements[spec.root]) return null;
  const components = uiCatalog.data.components as Record<string, { props: z.ZodTypeAny }>;
  for (const el of elements) {
    const def = components[el.type];
    if (!def || !def.props.safeParse(el.props ?? {}).success) return null;
    if (bindings((el as { on?: unknown }).on).some((b) => typeof b?.action !== "string" || !ACTIONS.has(b.action))) return null;
  }
  if (!validateSpec(spec).valid) return null;
  return spec;
}
