"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";

export type ChartSeries = { name: string; values: number[] };

export type ChartProps = {
  kind?: "bar" | "line" | "area" | "pie" | null;
  title?: string | null;
  labels: string[];
  series: ChartSeries[];
  unit?: string | null;
  stacked?: boolean | null;
};

const SLOTS = 5;
const PIE_SLICES = 4;
const OTHER = "rgb(var(--w-faint))";
const SURFACE = "rgb(var(--w-card))";
const GRID = "rgb(var(--w-line))";
const AXIS = "rgb(var(--w-line2))";
const INK = "rgb(var(--w-muted))";

const slot = (i: number) => `rgb(var(--w-chart-${(i % SLOTS) + 1}))`;

const compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 2 });
const exact = new Intl.NumberFormat("en", { maximumFractionDigits: 2 });

export function chartValue(value: number, unit?: string | null, short = false): string {
  const text = (short ? compact : exact).format(value);
  const u = unit?.trim();
  if (!u) return text;
  if (/^[$€£¥₹]$/.test(u)) return value < 0 ? `-${u}${text.slice(1)}` : `${u}${text}`;
  return u === "%" ? `${text}%` : `${text} ${u}`;
}

export function niceScale(low: number, high: number, wanted = 4): { min: number; max: number; ticks: number[] } {
  if (low === high) {
    const pad = Math.abs(low) || 1;
    low -= pad;
    high += pad;
  }
  const rough = (high - low) / wanted;
  const power = Math.pow(10, Math.floor(Math.log10(rough)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * power).find((s) => s >= rough) ?? power * 10;
  const min = Math.floor(low / step) * step;
  const max = Math.ceil(high / step) * step;
  const ticks: number[] = [];
  for (let v = min; v <= max + step / 2; v += step) ticks.push(Number(v.toFixed(10)));
  return { min, max, ticks };
}

function useBox<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [box, setBox] = useState({ width: 0, unit: 16 });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const read = () => setBox({ width: el.clientWidth, unit: parseFloat(getComputedStyle(document.documentElement).fontSize) || 16 });
    read();
    const observer = new ResizeObserver(read);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, box] as const;
}

function Swatch({ color, line }: { color: string; line?: boolean }) {
  return <span aria-hidden className={cn("inline-block w-2.5 shrink-0 rounded-[0.125rem]", line ? "h-[0.1875rem]" : "h-2.5")} style={{ background: color }} />;
}

function Legend({ items }: { items: { name: string; color: string; detail?: string }[] }) {
  return (
    <ul className="flex flex-wrap gap-x-3 gap-y-1 text-w-xs text-w-text2">
      {items.map((item, i) => (
        <li key={i} className="flex min-w-0 items-center gap-1.5">
          <Swatch color={item.color} />
          <span className="truncate">{item.name}</span>
          {item.detail && <span className="text-w-muted">{item.detail}</span>}
        </li>
      ))}
    </ul>
  );
}

function Tip({ x, width, title, rows }: { x: number; width: number; title: string; rows: { name: string; color: string; value: string }[] }) {
  const right = x > width / 2;
  return (
    <div
      role="status"
      className="pointer-events-none absolute top-1 z-10 max-w-[14rem] rounded-md border-2 border-w-ink bg-w-card px-2 py-1 text-w-xs shadow-[0_2px_0_rgb(var(--w-ink))]"
      style={right ? { right: width - x + 10 } : { left: x + 10 }}
    >
      <p className="truncate font-semibold text-w-strong">{title}</p>
      {rows.map((row, i) => (
        <p key={i} className="flex items-center gap-1.5 text-w-text2">
          <Swatch color={row.color} />
          <span className="truncate">{row.name}</span>
          <span className="ml-auto pl-2 font-semibold tabular-nums text-w-strong">{row.value}</span>
        </p>
      ))}
    </div>
  );
}

function DataTable({ labels, series, unit }: { labels: string[]; series: ChartSeries[]; unit?: string | null }) {
  return (
    <div className="max-h-[16rem] overflow-auto rounded-lg border-2 border-w-line">
      <table className="w-full text-left text-w-xs">
        <thead className="sticky top-0 bg-w-field">
          <tr>
            <th className="px-2 py-1 font-semibold" />
            {series.map((s, i) => (
              <th key={i} className="px-2 py-1 text-right font-semibold">{s.name}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {labels.map((label, r) => (
            <tr key={r} className="border-t border-w-line">
              <td className="px-2 py-1">{label}</td>
              {series.map((s, i) => (
                <td key={i} className="px-2 py-1 text-right tabular-nums">{chartValue(s.values[r] ?? 0, unit)}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function Plot({ kind, labels, series, unit, stacked, width, u }: { kind: "bar" | "line" | "area"; labels: string[]; series: ChartSeries[]; unit?: string | null; stacked: boolean; width: number; u: number }) {
  const [active, setActive] = useState<number | null>(null);
  const height = 13 * u;
  const count = labels.length;

  const scale = useMemo(() => {
    const totals = stacked ? labels.map((_, i) => series.reduce((sum, s) => sum + (s.values[i] ?? 0), 0)) : series.flatMap((s) => s.values);
    let low = Math.min(...totals);
    let high = Math.max(...totals);
    const wide = (high - low) / Math.max(Math.abs(high), 1e-9) > 0.6;
    if (kind !== "line" || low < 0 || wide) {
      low = Math.min(0, low);
      high = Math.max(0, high);
    }
    return niceScale(low, high);
  }, [kind, labels, series, stacked]);

  const tickText = scale.ticks.map((t) => chartValue(t, unit, true));
  const left = Math.max(...tickText.map((t) => t.length)) * 0.42 * u + 0.75 * u;
  const pad = { top: 0.5 * u, right: 0.5 * u, bottom: 1.5 * u, left };
  const innerW = Math.max(1, width - pad.left - pad.right);
  const innerH = height - pad.top - pad.bottom;
  const band = innerW / count;
  const y = (v: number) => pad.top + innerH - ((v - scale.min) / (scale.max - scale.min)) * innerH;
  const cx = (i: number) => pad.left + band * (i + 0.5);
  const zero = y(Math.max(scale.min, Math.min(0, scale.max)));

  const longest = Math.max(...labels.map((l) => l.length));
  const labelW = Math.min(longest, 12) * 0.42 * u + 0.5 * u;
  const every = Math.max(1, Math.ceil(labelW / band));
  const clip = (text: string) => (text.length > 12 ? `${text.slice(0, 11)}…` : text);

  const marks = () => {
    if (kind === "bar") {
      const groups = stacked ? 1 : series.length;
      const gap = 2;
      const barW = Math.max(1, Math.min(1.5 * u, (band * 0.72 - gap * (groups - 1)) / groups));
      const groupW = barW * groups + gap * (groups - 1);
      const radius = Math.min(4, barW / 2);
      return labels.map((_, i) => {
        let base = 0;
        return series.map((s, k) => {
          const v = s.values[i] ?? 0;
          const from = stacked ? base : 0;
          const to = from + v;
          if (stacked) base = to;
          const top = Math.min(y(from), y(to));
          const h = Math.max(0, Math.abs(y(to) - y(from)) - (stacked && k > 0 ? gap : 0));
          const x = cx(i) - groupW / 2 + (stacked ? 0 : k * (barW + gap));
          const tip = !stacked || k === series.length - 1;
          const r = tip ? Math.min(radius, h) : 0;
          const up = v >= 0;
          const d = up
            ? `M${x},${top + h}V${top + r}a${r},${r} 0 0 1 ${r},${-r}h${barW - 2 * r}a${r},${r} 0 0 1 ${r},${r}V${top + h}Z`
            : `M${x},${top}V${top + h - r}a${r},${r} 0 0 0 ${r},${r}h${barW - 2 * r}a${r},${r} 0 0 0 ${r},${-r}V${top}Z`;
          return <path key={`${i}-${k}`} d={d} fill={slot(k)} opacity={active === null || active === i ? 1 : 0.45} />;
        });
      });
    }
    let floor = labels.map(() => 0);
    return series.map((s, k) => {
      const tops = s.values.map((v, i) => (stacked ? floor[i] + v : v));
      const line = tops.map((v, i) => `${i ? "L" : "M"}${cx(i)},${y(v)}`).join("");
      const below = floor;
      const under = stacked
        ? below
            .map((v, i) => `L${cx(i)},${y(v)}`)
            .reverse()
            .join("")
        : `L${cx(count - 1)},${zero}L${cx(0)},${zero}`;
      if (stacked) floor = tops;
      const dots = count <= 12 || active !== null;
      return (
        <g key={k}>
          {kind === "area" && <path d={`${line}${under}Z`} fill={slot(k)} opacity={stacked ? 0.55 : 0.18} />}
          <path d={line} fill="none" stroke={slot(k)} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
          {dots &&
            tops.map((v, i) =>
              count <= 12 || active === i ? <circle key={i} cx={cx(i)} cy={y(v)} r={active === i ? 5 : 4} fill={slot(k)} stroke={SURFACE} strokeWidth={2} /> : null,
            )}
        </g>
      );
    });
  };

  const pick = (clientX: number, target: Element) => {
    const rect = target.getBoundingClientRect();
    const i = Math.floor((clientX - rect.left - pad.left) / band);
    setActive(i >= 0 && i < count ? i : null);
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    setActive((cur) => {
      const next = (cur ?? (e.key === "ArrowRight" ? -1 : count)) + (e.key === "ArrowRight" ? 1 : -1);
      return Math.max(0, Math.min(count - 1, next));
    });
  };

  return (
    <div className="relative w-full" style={{ height }}>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={`${kind} chart, ${count} points, ${series.map((s) => s.name).join(", ")}. Use the arrow keys to read values.`}
        tabIndex={0}
        className="absolute left-0 top-0 touch-pan-y rounded-md outline-none focus-visible:ring-2 focus-visible:ring-w-teal"
        onPointerMove={(e) => pick(e.clientX, e.currentTarget)}
        onPointerDown={(e) => pick(e.clientX, e.currentTarget)}
        onPointerLeave={() => setActive(null)}
        onBlur={() => setActive(null)}
        onKeyDown={onKey}
      >
        {scale.ticks.map((t, i) => (
          <g key={i}>
            <line x1={pad.left} x2={width - pad.right} y1={y(t)} y2={y(t)} stroke={t === 0 ? AXIS : GRID} strokeWidth={1} />
            <text x={pad.left - 0.375 * u} y={y(t)} textAnchor="end" dominantBaseline="middle" fontSize={0.6875 * u} fill={INK} className="tabular-nums">
              {tickText[i]}
            </text>
          </g>
        ))}
        {active !== null && kind !== "bar" && <line x1={cx(active)} x2={cx(active)} y1={pad.top} y2={pad.top + innerH} stroke={AXIS} strokeWidth={1} />}
        {marks()}
        {labels.map((label, i) =>
          i % every === 0 ? (
            <text key={i} x={cx(i)} y={height - 0.375 * u} textAnchor="middle" fontSize={0.6875 * u} fill={INK}>
              {clip(label)}
            </text>
          ) : null,
        )}
      </svg>
      {active !== null && (
        <Tip
          x={cx(active)}
          width={width}
          title={labels[active]}
          rows={series.map((s, k) => ({ name: s.name, color: slot(k), value: chartValue(s.values[active] ?? 0, unit) }))}
        />
      )}
    </div>
  );
}

function arc(cx: number, cy: number, outer: number, inner: number, from: number, to: number): string {
  const point = (r: number, a: number) => `${cx + r * Math.sin(a)},${cy - r * Math.cos(a)}`;
  const large = to - from > Math.PI ? 1 : 0;
  return `M${point(outer, from)}A${outer},${outer} 0 ${large} 1 ${point(outer, to)}L${point(inner, to)}A${inner},${inner} 0 ${large} 0 ${point(inner, from)}Z`;
}

function Pie({ slices, unit, u }: { slices: { name: string; value: number; color: string }[]; unit?: string | null; u: number }) {
  const [active, setActive] = useState<number | null>(null);
  const total = slices.reduce((sum, s) => sum + s.value, 0);
  const size = 9 * u;
  const outer = size / 2 - 2;
  const inner = outer * 0.58;
  let angle = 0;
  const shown = active !== null ? slices[active] : null;
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
      <svg width={size} height={size} role="img" aria-label={`Pie chart: ${slices.map((s) => `${s.name} ${chartValue(s.value, unit)}`).join(", ")}`} className="shrink-0" onPointerLeave={() => setActive(null)}>
        {total > 0 ? (
          slices.map((s, i) => {
            const from = angle;
            const sweep = (s.value / total) * Math.PI * 2;
            angle += sweep;
            if (sweep <= 0) return null;
            const d = sweep >= Math.PI * 2 - 1e-6 ? `${arc(size / 2, size / 2, outer, inner, 0, Math.PI)}${arc(size / 2, size / 2, outer, inner, Math.PI, Math.PI * 2)}` : arc(size / 2, size / 2, outer, inner, from, angle);
            return <path key={i} d={d} fill={s.color} stroke={SURFACE} strokeWidth={2} opacity={active === null || active === i ? 1 : 0.45} onPointerEnter={() => setActive(i)} onPointerDown={() => setActive(i)} />;
          })
        ) : (
          <circle cx={size / 2} cy={size / 2} r={(outer + inner) / 2} fill="none" stroke={GRID} strokeWidth={outer - inner} />
        )}
        <text x={size / 2} y={size / 2 - 0.25 * u} textAnchor="middle" fontSize={0.9375 * u} fontWeight={700} fill="rgb(var(--w-strong))" className="tabular-nums">
          {chartValue(shown ? shown.value : total, unit, true)}
        </text>
        <text x={size / 2} y={size / 2 + 0.75 * u} textAnchor="middle" fontSize={0.6875 * u} fill={INK}>
          {shown ? (shown.name.length > 14 ? `${shown.name.slice(0, 13)}…` : shown.name) : "Total"}
        </text>
      </svg>
      <ul className="min-w-[8rem] flex-1 space-y-1 text-w-xs text-w-text2">
        {slices.map((s, i) => (
          <li key={i} className={cn("flex items-center gap-1.5", active !== null && active !== i && "opacity-50")} onPointerEnter={() => setActive(i)} onPointerLeave={() => setActive(null)}>
            <Swatch color={s.color} />
            <span className="truncate">{s.name}</span>
            <span className="ml-auto pl-2 font-semibold tabular-nums text-w-strong">{chartValue(s.value, unit)}</span>
            <span className="w-9 text-right tabular-nums text-w-muted">{total > 0 ? `${Math.round((s.value / total) * 100)}%` : ""}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export function pieSlices(labels: string[], values: number[]): { name: string; value: number; color: string }[] {
  const all = labels.map((name, i) => ({ name, value: Math.max(0, values[i] ?? 0) }));
  if (all.length <= PIE_SLICES + 1) return all.map((s, i) => ({ ...s, color: slot(i) }));
  const sorted = [...all].sort((a, b) => b.value - a.value);
  const top = sorted.slice(0, PIE_SLICES).map((s, i) => ({ ...s, color: slot(i) }));
  const rest = sorted.slice(PIE_SLICES).reduce((sum, s) => sum + s.value, 0);
  return [...top, { name: `Other (${sorted.length - PIE_SLICES})`, value: rest, color: OTHER }];
}

export function ChartView({ kind, title, labels, series, unit, stacked }: ChartProps) {
  const [ref, box] = useBox<HTMLDivElement>();
  const [table, setTable] = useState(false);
  const mode = kind ?? "bar";
  const clean = useMemo(() => series.slice(0, SLOTS).map((s) => ({ name: s.name, values: labels.map((_, i) => (Number.isFinite(s.values[i]) ? s.values[i] : 0)) })), [labels, series]);
  const stack = !!stacked && mode !== "line" && mode !== "pie" && clean.length > 1 && clean.every((s) => s.values.every((v) => v >= 0));
  const u = box.unit;
  return (
    <figure className="min-w-0 rounded-lg border-2 border-w-line bg-w-card p-3">
      <div className="mb-2 flex items-start gap-2">
        <figcaption className="min-w-0 flex-1 text-w-sm font-semibold leading-tight text-w-strong">{title?.trim() || (clean.length === 1 ? clean[0].name : "")}</figcaption>
        <button type="button" onClick={() => setTable((t) => !t)} aria-pressed={table} className="shrink-0 rounded-md px-1.5 text-w-xs font-semibold text-w-muted hover:bg-w-field hover:text-w-strong">
          {table ? "Chart" : "Table"}
        </button>
      </div>
      <div ref={ref} className="flex w-full min-w-0 flex-col gap-2 overflow-hidden">
        {table ? (
          <DataTable labels={labels} series={clean} unit={unit} />
        ) : mode === "pie" ? (
          <Pie slices={pieSlices(labels, clean[0]?.values ?? [])} unit={unit} u={u} />
        ) : (
          <>
            {clean.length > 1 && <Legend items={clean.map((s, k) => ({ name: s.name, color: slot(k) }))} />}
            {box.width > 0 && <Plot kind={mode} labels={labels} series={clean} unit={unit} stacked={stack} width={box.width} u={u} />}
          </>
        )}
      </div>
    </figure>
  );
}
