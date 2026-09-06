"use client";

import { useEffect, useState, type ReactNode } from "react";
import * as SelectPrimitive from "@radix-ui/react-select";
import * as PopoverPrimitive from "@radix-ui/react-popover";
import { Command } from "cmdk";
import { CaretDown, CaretUpDown, Check, Trash } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { CHUNKY } from "./styles";
import Glide from "./glide";

const TRIGGER =
  "group inline-flex h-10 min-w-0 items-center justify-between gap-2 rounded-lg border-2 border-w-ink bg-w-field px-3 text-left text-w-sm text-w-text outline-none transition-[background-color,box-shadow] duration-150 hover:bg-w-paper focus-visible:ring-2 focus-visible:ring-w-teal data-[state=open]:bg-w-paper data-[state=open]:ring-2 data-[state=open]:ring-w-teal/60 data-[placeholder]:text-w-faint disabled:pointer-events-none disabled:opacity-50";
const CARET = "shrink-0 text-w-muted transition-transform duration-200 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] group-data-[state=open]:rotate-180 motion-reduce:transition-none";
export const MENU =
  "z-[60] overflow-hidden rounded-xl border-2 border-w-ink bg-w-paper p-1.5 text-w-text shadow-[0_4px_0_rgb(var(--w-ink)),0_16px_36px_-14px_rgb(0_0_0/0.45)] data-[state=open]:animate-w-menu-in data-[state=closed]:animate-w-menu-out motion-reduce:!animate-none";
const CONTENT = `${MENU} max-h-[min(22rem,var(--radix-select-content-available-height))] min-w-[var(--radix-select-trigger-width)] origin-[var(--radix-select-content-transform-origin)]`;
export const MENU_ITEM =
  "relative flex cursor-pointer select-none items-center gap-2.5 rounded-lg py-2 pl-8 pr-3 text-w-sm text-w-text2 outline-none transition-colors duration-100 data-[disabled]:pointer-events-none data-[highlighted]:bg-w-sand data-[selected=true]:bg-w-sand data-[highlighted]:text-w-strong data-[selected=true]:text-w-strong data-[state=checked]:font-semibold data-[state=checked]:text-w-strong data-[disabled]:opacity-40";
const ITEM = `${MENU_ITEM} data-[highlighted]:bg-transparent data-[selected=true]:bg-transparent`;
const PILL = "rounded-lg bg-w-sand";

export interface Option {
  value: string;
  label: string;
  hint?: string;
}

export function WSelect({
  value,
  onChange,
  options,
  placeholder,
  className,
  size = "md",
  ariaLabel,
  disabled,
  icon,
}: {
  value: string;
  onChange: (value: string) => void;
  options: Option[];
  placeholder?: string;
  className?: string;
  size?: "sm" | "md";
  ariaLabel?: string;
  disabled?: boolean;
  icon?: ReactNode;
}) {
  const sentinel = "__none__";
  return (
    <SelectPrimitive.Root disabled={disabled} value={value === "" ? (options.some((o) => o.value === "") ? sentinel : "") : value} onValueChange={(v) => onChange(v === sentinel ? "" : v)}>
      <SelectPrimitive.Trigger disabled={disabled} aria-label={ariaLabel} className={cn(TRIGGER, size === "sm" && "h-8 rounded-md px-2 text-w-xs", className)}>
        <span className={cn("flex min-w-0 items-center", size === "sm" ? "gap-1.5" : "gap-2")}>
          {icon && <span aria-hidden className="grid shrink-0 place-items-center text-w-muted">{icon}</span>}
          <span className="truncate">
            <SelectPrimitive.Value placeholder={placeholder} />
          </span>
        </span>
        <SelectPrimitive.Icon>
          <CaretDown size={size === "sm" ? 12 : 14} weight="bold" className={CARET} />
        </SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal>
        <SelectPrimitive.Content position="popper" sideOffset={6} className={CONTENT}>
          <SelectPrimitive.Viewport className="relative">
            <Glide target="[data-highlighted]" className={PILL} />
            {options.map((o) => (
              <SelectPrimitive.Item key={o.value || sentinel} value={o.value || sentinel} className={ITEM}>
                <SelectPrimitive.ItemIndicator className="absolute left-2.5 inline-flex">
                  <Check size={14} weight="bold" className="text-w-link" />
                </SelectPrimitive.ItemIndicator>
                <SelectPrimitive.ItemText>{o.label}</SelectPrimitive.ItemText>
                {o.hint && <span className="ml-auto pl-3 text-w-xs text-w-muted">{o.hint}</span>}
              </SelectPrimitive.Item>
            ))}
          </SelectPrimitive.Viewport>
        </SelectPrimitive.Content>
      </SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  );
}

export function WCombobox({
  value,
  onChange,
  options,
  placeholder,
  className,
  allowCustom = true,
  ariaLabel,
  placeholderIsValue = false,
}: {
  value: string;
  onChange: (value: string) => void;
  options: Option[];
  placeholder?: string;
  className?: string;
  allowCustom?: boolean;
  ariaLabel?: string;
  placeholderIsValue?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const typed = query.trim();
  const exact = options.some((o) => o.value === typed);
  const pick = (v: string) => {
    onChange(v);
    setOpen(false);
    setQuery("");
  };
  const current = options.find((o) => o.value === value);
  return (
    <PopoverPrimitive.Root open={open} onOpenChange={setOpen}>
      <PopoverPrimitive.Trigger asChild>
        <button type="button" aria-label={ariaLabel} className={cn(TRIGGER, "w-full", className)}>
          <span className={cn("truncate", !value && !placeholderIsValue && "text-w-faint")}>{current?.label ?? (value || placeholder)}</span>
          <CaretUpDown size={14} weight="bold" className="shrink-0 text-w-muted" />
        </button>
      </PopoverPrimitive.Trigger>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          sideOffset={6}
          align="start"
          className={cn(MENU, "w-[var(--radix-popover-trigger-width)] min-w-[14rem] origin-[var(--radix-popover-content-transform-origin)] p-0")}
        >
          <Command loop>
            <Command.Input
              value={query}
              onValueChange={setQuery}
              placeholder={allowCustom ? "Search or type any name" : "Search"}
              className="w-full border-b-2 border-dashed border-w-line2 bg-transparent px-3.5 py-2.5 text-w-sm outline-none placeholder:text-w-faint"
            />
            <Command.List className="max-h-64 overflow-y-auto p-1.5 [&_[cmdk-list-sizer]]:relative">
              <Glide target={"[cmdk-item][data-selected=\"true\"]"} className={PILL} />
              <Command.Empty className="px-3 py-2 text-w-sm text-w-muted">{allowCustom ? "Press enter to use what you typed." : "Nothing matches."}</Command.Empty>
              {options.map((o) => (
                <Command.Item
                  key={o.value}
                  value={`${o.value} ${o.label}`}
                  onSelect={() => pick(o.value)}
                  className={cn(ITEM, o.value === value && "font-semibold text-w-strong")}
                >
                  {o.value === value && <Check size={14} weight="bold" className="absolute left-2.5 text-w-link" />}
                  {o.label}
                  {o.hint && <span className="ml-auto pl-3 text-w-xs text-w-muted">{o.hint}</span>}
                </Command.Item>
              ))}
              {allowCustom && typed && !exact && (
                <Command.Item
                  value={`__custom__ ${typed}`}
                  onSelect={() => pick(typed)}
                  className={cn(ITEM, "pl-3")}
                >
                  Use <span className="font-mono">{typed}</span>
                </Command.Item>
              )}
            </Command.List>
          </Command>
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}

export function ConfirmDelete({ label, onConfirm, className, size = 13 }: { label: string; onConfirm: () => void; className?: string; size?: number }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const id = window.setTimeout(() => setArmed(false), 3000);
    return () => window.clearTimeout(id);
  }, [armed]);
  return (
    <button
      type="button"
      aria-label={armed ? `Click again to confirm: ${label}` : label}
      title={armed ? "Click again to confirm" : label}
      onClick={() => {
        if (!armed) return setArmed(true);
        setArmed(false);
        onConfirm();
      }}
      onBlur={() => setArmed(false)}
      className={cn(
        CHUNKY,
        "flex items-center gap-1 p-1 text-w-xs font-semibold",
        armed ? "bg-w-alert px-2 text-w-onalert" : "bg-w-card hover:bg-w-sand",
        className,
      )}
    >
      <Trash size={size} weight="bold" />
      {armed && "Delete?"}
    </button>
  );
}
