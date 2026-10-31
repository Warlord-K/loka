import { cn } from "@/lib/utils";

const TONES = {
  paper: "border-w-line2 border-b-w-ink/50 bg-w-card text-w-text2",
  scrim: "border-w-onscrim/25 border-b-w-onscrim/45 bg-w-onscrim/10 text-w-onscrim",
} as const;

const SIZES = {
  sm: "h-5 min-w-5 px-1 text-w-3xs",
  md: "h-6 min-w-6 px-1.5 text-w-2xs",
} as const;

export function Keycap({ children, tone = "paper", size = "md", className }: { children: React.ReactNode; tone?: keyof typeof TONES; size?: keyof typeof SIZES; className?: string }) {
  return (
    <kbd
      className={cn(
        "inline-grid shrink-0 place-items-center rounded-md border border-b-[3px] font-sans font-semibold not-italic leading-none tracking-normal",
        TONES[tone],
        SIZES[size],
        className,
      )}
    >
      {children}
    </kbd>
  );
}

export function Keys({ keys, tone, size, className }: { keys: string[]; tone?: keyof typeof TONES; size?: keyof typeof SIZES; className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-1", className)}>
      {keys.map((k, i) => (
        <Keycap key={`${k}-${i}`} tone={tone} size={size}>{k}</Keycap>
      ))}
    </span>
  );
}

export function hintKeys(hint: string): string[] {
  const parts = hint.trim().split(/\s+/);
  if (parts.length > 1) return parts;
  const mods = hint.match(/^([⌘⌥⇧⌃]+)(.+)$/);
  return mods ? [...mods[1].split(""), mods[2]] : [hint];
}

export function isMac(): boolean {
  return typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
}
