"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import Glide from "./glide";
import { AnimatePresence, motion } from "framer-motion";
import { CaretRight, Info, MagnifyingGlass, X, type Icon } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { EASE, FOCUS, PANEL, PIXEL } from "./styles";
import { EASE_SMOOTH } from "./motion";
import { Keycap } from "./keycap";
import { scrollWithin } from "@/lib/scroll";

export const SEARCH_ROW_FOCUS = {
  "aria-current": true as const,
  tabIndex: -1,
  ref(node: HTMLElement | null) {
    if (!node) return;
    const frame = window.requestAnimationFrame(() => {
      node.focus({ preventScroll: true });
      scrollWithin(node, { block: "center" });
    });
    return () => window.cancelAnimationFrame(frame);
  },
};
export const SEARCH_ROW_RING = "aria-[current=true]:ring-2 aria-[current=true]:ring-inset aria-[current=true]:ring-w-teal";

export interface SettingsNavItem {
  id: string;
  label: string;
  group: string;
  icon: Icon;
  keywords?: string;
  badge?: React.ReactNode;
}

export const CLOSE_BTN = `grid h-10 w-10 shrink-0 place-items-center rounded-lg border-2 border-w-ink bg-w-card text-w-text shadow-[0_2px_0_rgb(var(--w-ink))] transition-[transform,box-shadow,background-color] ${EASE} hover:bg-w-sand active:translate-y-[2px] active:shadow-none disabled:pointer-events-none disabled:opacity-50 motion-reduce:transform-none ${FOCUS}`;
export const GHOST_ICON_BTN = `grid h-10 w-10 shrink-0 place-items-center rounded-lg text-w-muted transition-colors ${EASE} hover:bg-w-sand hover:text-w-strong disabled:opacity-50 ${FOCUS} focus-visible:ring-offset-0`;

export function CloseButton({ onClose, label = "Close", title, className }: { onClose: () => void; label?: string; title?: string; className?: string }) {
  return (
    <button type="button" onClick={onClose} aria-label={label} title={title} className={cn(CLOSE_BTN, className)}>
      <X size={16} weight="bold" />
    </button>
  );
}

export function WindowHeader({
  title,
  subtitle,
  icon: HeadIcon,
  leading,
  actions,
  onClose,
  closeTitle,
  className,
}: {
  title: React.ReactNode;
  subtitle?: React.ReactNode;
  leading?: React.ReactNode;
  icon?: Icon;
  actions?: React.ReactNode;
  onClose?: () => void;
  closeTitle?: string;
  className?: string;
}) {
  return (
    <header className={cn("flex shrink-0 items-center gap-3 border-b-2 border-dashed border-w-line2 px-4 py-3.5 sm:px-5", className)}>
      {leading}
      {HeadIcon && (
        <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg border-2 border-w-ink bg-w-aqua text-w-link shadow-[0_2px_0_rgb(var(--w-ink))]">
          <HeadIcon size={20} weight="duotone" />
        </span>
      )}
      <div className="min-w-0 flex-1">
        <h2 className={cn(PIXEL, "truncate text-w-xl leading-tight text-w-strong")}>{title}</h2>
        {subtitle && <p className="mt-0.5 line-clamp-2 text-w-xs text-w-muted sm:line-clamp-1">{subtitle}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      {onClose && <CloseButton onClose={onClose} title={closeTitle} />}
    </header>
  );
}

export function SettingsShell({
  title,
  items,
  active,
  onSelect,
  onClose,
  footer,
  children,
}: {
  title: string;
  footer?: React.ReactNode;
  items: SettingsNavItem[];
  active: string;
  onSelect: (id: string) => void;
  onClose: () => void;
  children: React.ReactNode;
}) {
  const current = items.find((i) => i.id === active) ?? items[0];
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return items;
    return items.filter((i) => `${i.label} ${i.group} ${i.keywords ?? ""}`.toLowerCase().includes(q));
  }, [items, query]);
  const groups = shown.reduce<{ name: string; items: SettingsNavItem[] }[]>((acc, item) => {
    const group = acc.find((g) => g.name === item.group);
    if (group) group.items.push(item);
    else acc.push({ name: item.group, items: [item] });
    return acc;
  }, []);
  const scroller = useRef<HTMLDivElement>(null);
  const strip = useRef<HTMLDivElement>(null);
  const navList = useRef<HTMLDivElement>(null);
  useEffect(() => {
    scroller.current?.scrollTo({ top: 0 });
    scrollWithin(navList.current?.querySelector(`[data-side="${CSS.escape(active)}"]`), { block: "nearest", smooth: true });
    const chip = strip.current?.querySelector<HTMLElement>(`[data-nav="${CSS.escape(active)}"]`);
    if (chip && strip.current) strip.current.scrollTo({ left: chip.offsetLeft - 16 });
  }, [active]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLElement && (e.target.closest("input, textarea, select, [contenteditable=true]") !== null);
      if (e.key === "/" && !typing) {
        e.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div role="dialog" aria-modal="true" aria-label={title} className={cn(PANEL, "@container pointer-events-auto mx-auto flex h-full w-full overflow-hidden")}>
      <nav aria-label={`${title} sections`} className="hidden w-64 shrink-0 flex-col border-r-2 border-dashed border-w-line2 bg-w-sand/50 @3xl:flex">
        <div className="p-3">
          <label className="flex h-10 items-center gap-2 rounded-lg border-2 border-w-line2 bg-w-paper px-2.5 transition-colors focus-within:border-w-ink focus-within:ring-2 focus-within:ring-w-teal">
            <MagnifyingGlass size={15} weight="bold" className="shrink-0 text-w-muted" />
            <input
              ref={searchRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && shown[0]) {
                  onSelect(shown[0].id);
                  setQuery("");
                }
                if (e.key === "Escape" && query) {
                  e.preventDefault();
                  e.stopPropagation();
                  setQuery("");
                }
              }}
              placeholder="Search"
              aria-label={`Search ${title.toLowerCase()} sections`}
              className="h-full min-w-0 flex-1 bg-transparent text-w-sm text-w-text outline-none placeholder:text-w-muted"
            />
            {!query && <Keycap size="sm">/</Keycap>}
          </label>
        </div>
        <div ref={navList} className="relative min-h-0 flex-1 space-y-4 overflow-y-auto px-3 pb-4 [&_button]:scroll-my-3">
          <Glide hover=".nav-off" className="rounded-lg bg-w-paper/70" />
          {groups.length === 0 && <p className="px-2.5 py-2 text-w-sm text-w-muted">Nothing matches.</p>}
          {groups.map((group, gi) => (
            <div key={group.name}>
              {gi > 0 || group.name !== items[0].group ? <p className="mb-1 px-2.5 text-w-xs font-medium text-w-muted">{group.name}</p> : null}
              <ul className="space-y-0.5">
                {group.items.map((item) => {
                  const on = item.id === current.id;
                  return (
                    <li key={item.id}>
                      <button
                        type="button"
                        onClick={() => {
                          onSelect(item.id);
                          setQuery("");
                        }}
                        data-side={item.id}
                        aria-current={on ? "page" : undefined}
                        className={cn(
                          "relative flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-w-sm transition-colors",
                          EASE,
                          FOCUS,
                          "focus-visible:ring-offset-0",
                          on ? "bg-w-paper font-semibold text-w-strong ring-2 ring-inset ring-w-line2" : "nav-off text-w-text2 hover:text-w-text",
                        )}
                      >
                        <item.icon size={17} weight={on ? "duotone" : "bold"} className={cn("shrink-0", on ? "text-w-link" : "text-w-muted")} />
                        <span className="min-w-0 flex-1 truncate">{item.label}</span>
                        {item.badge}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}
        </div>
        {footer && <div className="border-t-2 border-dashed border-w-line2 p-3">{footer}</div>}
      </nav>

      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-[3.75rem] shrink-0 items-center gap-3 border-b-2 border-dashed border-w-line2 pl-4 pr-3 @3xl:pl-8">
          <nav aria-label="Breadcrumb" className="min-w-0 flex-1">
            <ol className="flex min-w-0 items-center gap-2 text-w-base">
              <li className="shrink-0">
                <button type="button" onClick={() => onSelect(items[0].id)} className={cn("rounded text-w-muted transition-colors hover:text-w-text", FOCUS, "focus-visible:ring-offset-0")}>
                  {title}
                </button>
              </li>
              <li aria-hidden className="shrink-0 text-w-faint">
                <CaretRight size={12} weight="bold" />
              </li>
              <li aria-current="page" className="truncate font-semibold text-w-strong">
                {current.label}
              </li>
            </ol>
          </nav>
          <CloseButton onClose={onClose} />
        </header>

        <div ref={strip} className="flex shrink-0 gap-1 overflow-x-auto border-b-2 border-dashed border-w-line2 bg-w-sand/50 px-3 py-2 [scrollbar-width:none] @3xl:hidden" aria-label={`${title} sections`}>
          {items.map((item) => {
            const on = item.id === current.id;
            return (
              <button
                key={item.id}
                type="button"
                data-nav={item.id}
                aria-current={on ? "page" : undefined}
                onClick={() => onSelect(item.id)}
                className={cn(
                  "flex shrink-0 items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-w-xs font-semibold transition-colors",
                  EASE,
                  FOCUS,
                  "focus-visible:ring-offset-0",
                  on ? "bg-w-paper text-w-strong ring-2 ring-inset ring-w-line2" : "text-w-text2 hover:bg-w-paper/60",
                )}
              >
                <item.icon size={14} weight={on ? "duotone" : "bold"} />
                {item.label}
              </button>
            );
          })}
        </div>

        <div ref={scroller} className="min-h-0 flex-1 overflow-y-auto">
          <AnimatePresence mode="wait" initial={false}>
            <motion.div
              key={current.id}
              initial={{ opacity: 0, y: 6 }}
              animate={{ opacity: 1, y: 0, transition: { duration: 0.2, ease: EASE_SMOOTH } }}
              exit={{ opacity: 0, transition: { duration: 0.1, ease: EASE_SMOOTH } }}
              className="mx-auto w-full max-w-3xl space-y-8 px-4 pb-10 pt-6 @3xl:px-8 @3xl:pt-8 @7xl:max-w-5xl"
            >
              {children}
            </motion.div>
          </AnimatePresence>
        </div>
      </div>
    </div>
  );
}

export function SettingsGroup({ title, description, action, children, id }: { title?: string; description?: React.ReactNode; action?: React.ReactNode; children: React.ReactNode; id?: string }) {
  return (
    <section id={id} className="space-y-3">
      {(title || action) && (
        <div className="flex items-end gap-3 px-1">
          <div className="min-w-0 flex-1">
            {title && <h3 className="text-w-lg font-semibold text-w-strong">{title}</h3>}
            {description && <p className="mt-0.5 text-w-sm leading-relaxed text-w-muted">{description}</p>}
          </div>
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

export function SettingsCard({ children, className, rows = false }: { children: React.ReactNode; className?: string; rows?: boolean }) {
  return <div className={cn("rounded-xl border-2 border-w-line bg-w-card/70", rows ? "divide-y-2 divide-w-line" : "p-4", className)}>{children}</div>;
}

export function SettingsRow({ title, description, control, children, htmlFor }: { title: React.ReactNode; description?: React.ReactNode; control?: React.ReactNode; children?: React.ReactNode; htmlFor?: string }) {
  const Label = htmlFor ? "label" : "div";
  return (
    <div className="px-4 py-3.5">
      <div className="flex flex-col gap-2.5 @lg:flex-row @lg:items-center @lg:gap-6">
        <Label htmlFor={htmlFor} className="min-w-0 flex-1">
          <span className="block text-w-sm font-medium text-w-text">{title}</span>
          {description && <span className="mt-0.5 block text-w-xs leading-relaxed text-w-muted">{description}</span>}
        </Label>
        {control && <div className="flex shrink-0 items-center gap-2">{control}</div>}
      </div>
      {children && <div className="mt-3">{children}</div>}
    </div>
  );
}

export function Switch({ checked, onChange, label, disabled, id }: { checked: boolean; onChange: (next: boolean) => void; label: string; disabled?: boolean; id?: string }) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "group/switch relative inline-flex h-6 w-11 shrink-0 items-center rounded-md border-2 border-w-ink shadow-[inset_0_2px_0_rgb(var(--w-ink)/0.12)] transition-colors disabled:cursor-not-allowed disabled:opacity-50",
        EASE,
        FOCUS,
        checked ? "bg-w-teal" : "bg-w-field",
      )}
    >
      <span
        className={cn(
          "h-4 w-4 rounded-[0.25rem] border-2 border-w-ink bg-w-paper shadow-[0_2px_0_rgb(var(--w-ink))] transition-[transform,width] duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] group-active/switch:w-5 motion-reduce:transition-none",
          checked ? "translate-x-[1.3rem] group-active/switch:translate-x-[1.05rem]" : "translate-x-0.5",
        )}
      />
    </button>
  );
}

export function EmptyState({
  icon: StateIcon,
  title,
  description,
  action,
  className,
  size = "md",
}: {
  icon: Icon;
  title: string;
  description?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
  size?: "md" | "lg";
}) {
  if (size === "lg")
    return (
      <div className={cn("flex flex-col items-center px-6 py-14 text-center motion-safe:animate-w-rise", className)}>
        <span className="relative grid h-14 w-14 -rotate-3 place-items-center rounded-xl border-2 border-w-ink bg-w-card text-w-text2 shadow-[0_3px_0_rgb(var(--w-ink))]">
          <StateIcon size={26} weight="duotone" />
        </span>
        <p className="mt-5 text-w-base font-semibold text-w-strong">{title}</p>
        {description && <p className="mt-1.5 max-w-sm text-w-sm leading-relaxed text-w-muted">{description}</p>}
        {action && <div className="mt-5">{action}</div>}
      </div>
    );
  return (
    <div className={cn("flex items-center gap-3 rounded-xl border-2 border-dashed border-w-line2 px-4 py-3.5", className)}>
      <span className="grid h-10 w-10 shrink-0 -rotate-3 place-items-center rounded-lg border-2 border-w-ink bg-w-paper text-w-text2 shadow-[0_2px_0_rgb(var(--w-ink))]">
        <StateIcon size={20} weight="duotone" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-w-sm font-medium text-w-text">{title}</p>
        {description && <p className="mt-0.5 text-w-xs leading-relaxed text-w-muted">{description}</p>}
      </div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}

export function InfoNote({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <p className={cn("flex items-start gap-2 text-w-xs leading-relaxed text-w-muted", className)}>
      <Info size={14} weight="bold" className="mt-0.5 shrink-0" />
      <span>{children}</span>
    </p>
  );
}
