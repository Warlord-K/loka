export const FOCUS =
  "outline-none focus-visible:ring-2 focus-visible:ring-w-teal focus-visible:ring-offset-2 focus-visible:ring-offset-w-paper";
export const EASE = "duration-150 [transition-timing-function:cubic-bezier(0.2,0.8,0.2,1)]";

export const PANEL =
  "rounded-xl border-[3px] border-w-ink bg-w-paper text-w-text shadow-[0_5px_0_rgb(var(--w-ink))] ring-2 ring-inset ring-w-line";
export const CHUNKY = `rounded-lg border-2 border-w-ink shadow-[0_3px_0_rgb(var(--w-ink))] transition-[transform,box-shadow,background-color,color,opacity] ${EASE} active:translate-y-[3px] active:shadow-none disabled:pointer-events-none disabled:opacity-50 motion-reduce:transform-none ${FOCUS}`;

export const BTN = `inline-flex select-none items-center justify-center gap-1.5 whitespace-nowrap font-semibold ${CHUNKY}`;
export const BTN_MD = "h-10 px-4 text-w-sm";
export const BTN_SM = "h-8 px-3 text-w-xs";
export const BTN_PRIMARY = `${BTN} ${BTN_MD} bg-w-teal text-w-strong hover:bg-w-teal2`;
export const BTN_SECONDARY = `${BTN} ${BTN_MD} bg-w-card text-w-text hover:bg-w-sand`;
export const BTN_PRIMARY_SM = `${BTN} ${BTN_SM} bg-w-teal text-w-strong hover:bg-w-teal2`;
export const BTN_SECONDARY_SM = `${BTN} ${BTN_SM} bg-w-card text-w-text hover:bg-w-sand`;
export const BTN_DANGER = `${BTN} ${BTN_MD} bg-w-rose text-w-danger hover:bg-w-coral/30`;
export const BTN_GHOST = `inline-flex items-center justify-center gap-1.5 rounded-lg px-2.5 py-1.5 text-w-sm text-w-text2 transition-colors ${EASE} hover:bg-w-sand hover:text-w-text disabled:pointer-events-none disabled:opacity-50 ${FOCUS}`;
export const BTN_SECONDARY_DANGER = `${BTN} ${BTN_MD} bg-w-card text-w-danger hover:bg-w-rose`;
export const BTN_DANGER_GHOST = `inline-flex items-center justify-center gap-1.5 rounded-lg px-2.5 py-1.5 text-w-sm text-w-danger transition-colors ${EASE} hover:bg-w-rose disabled:pointer-events-none disabled:opacity-50 ${FOCUS}`;
export const ICON_BTN_MD = `${CHUNKY} grid h-10 w-10 shrink-0 place-items-center bg-w-card text-w-text hover:bg-w-sand`;
export const ICON_BTN = `${CHUNKY} grid h-8 w-8 shrink-0 place-items-center bg-w-card text-w-text hover:bg-w-sand [@media(pointer:coarse)]:h-10 [@media(pointer:coarse)]:w-10`;

export const TABS =
  "flex shrink-0 gap-1 overflow-x-auto border-b-2 border-dashed border-w-line2 px-3 [scrollbar-width:none] sm:px-5";
export const TAB = `relative flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-t-md px-2.5 py-3 text-w-sm font-medium transition-colors after:absolute after:inset-x-1.5 after:bottom-0 after:h-[2px] after:rounded-full after:transition-colors ${EASE} ${FOCUS} focus-visible:ring-inset focus-visible:ring-offset-0`;
export const TAB_ON = "text-w-strong after:bg-w-teal [&_svg]:text-w-link";
export const TAB_OFF = "text-w-muted after:bg-transparent hover:text-w-text hover:after:bg-w-line2";
export const TAB_COUNT = "grid h-[1.125rem] min-w-[1.125rem] place-items-center rounded-[0.3rem] bg-w-sand px-1.5 text-w-3xs font-semibold tabular-nums text-w-text2";

export const SEGMENT = "inline-flex rounded-lg border-2 border-w-ink bg-w-card p-0.5 shadow-[0_2px_0_rgb(var(--w-ink))]";
export const SEGMENT_ITEM = `rounded-md px-2.5 py-1 text-w-xs font-semibold transition-colors ${EASE} ${FOCUS} focus-visible:ring-offset-0`;
export const SEGMENT_ON = "bg-w-invert text-w-oninvert";
export const SEGMENT_OFF = "text-w-text2 hover:bg-w-sand hover:text-w-text";

export const PILL = "inline-flex items-center gap-1 rounded-[0.3rem] border-[1.5px] px-1.5 py-[0.1875rem] text-w-2xs font-semibold leading-none [&_svg]:shrink-0";
export const PILL_NEUTRAL = `${PILL} border-w-line2 bg-w-field text-w-text2`;
export const PILL_INK = `${PILL} border-w-ink bg-w-invert text-w-oninvert`;
export const PILL_OK = `${PILL} border-w-ok/55 bg-w-mint text-w-ok`;
export const PILL_WARN = `${PILL} border-w-warn/55 bg-w-lemon text-w-warn`;
export const PILL_DANGER = `${PILL} border-w-danger/55 bg-w-rose text-w-danger`;
export const PILL_INFO = `${PILL} border-w-teal/55 bg-w-aqua text-w-link`;

export const HUD_CHIP = `rounded-lg border-2 border-w-ink bg-w-paper/95 shadow-[0_3px_0_rgb(var(--w-ink))] transition-[transform,box-shadow,background-color] ${EASE} hover:bg-w-sand active:translate-y-[3px] active:shadow-none motion-reduce:transform-none ${FOCUS}`;
export const HUD_ICON = `grid h-11 w-11 place-items-center ${HUD_CHIP}`;
export const HUD_GROUP = "flex items-stretch divide-x-2 divide-w-line overflow-hidden rounded-lg border-2 border-w-ink bg-w-paper/95 text-w-text shadow-[0_3px_0_rgb(var(--w-ink))]";
export const HUD_SEG_BASE = `grid h-10 w-11 place-items-center transition-colors ${EASE} outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-w-teal`;
export const HUD_SEG = `${HUD_SEG_BASE} hover:bg-w-field focus-visible:bg-w-field`;
export const HUD_SEG_ON = `${HUD_SEG_BASE} bg-w-invert text-w-oninvert`;
export const BADGE =
  "absolute -right-2 -top-2 grid h-5 min-w-5 place-items-center rounded-md border-2 border-w-ink bg-w-alert px-1 text-w-2xs font-bold tabular-nums text-w-onalert animate-w-pop motion-reduce:animate-none";
export const TAG_X = "inline-grid min-w-[1.375rem] place-items-center rounded-[0.3rem] border-[1.5px] border-w-ink bg-w-invert px-1 py-[0.125rem] font-mono text-w-3xs font-bold leading-none tabular-nums text-w-oninvert";
export const PIXEL = "font-[family-name:var(--font-pixel)]";

export const TITLE = "truncate text-w-lg font-semibold leading-tight text-w-strong";

export const INPUT = `min-w-0 flex-1 rounded-lg border-2 border-w-ink bg-w-field px-3 py-2 text-w-base outline-none transition-shadow ${EASE} placeholder:text-w-faint focus:ring-2 focus:ring-w-teal disabled:opacity-60`;
export const CARD = `rounded-lg border-2 border-w-ink bg-w-card transition-[transform,box-shadow,background-color] ${EASE}`;

export const SURFACE = "rounded-xl border-2 border-w-line bg-w-card/70";
export const ITEM_CARD = `rounded-lg border-2 border-w-line bg-w-card shadow-[0_1px_0_rgb(var(--w-line))] transition-[border-color,box-shadow,background-color,opacity] ${EASE} hover:border-w-line2 hover:shadow-[0_2px_0_rgb(var(--w-line2))]`;
export const SECTION_TITLE = "text-w-sm font-semibold text-w-strong";
export const COUNT = "inline-grid h-5 min-w-5 place-items-center rounded-[0.3rem] bg-w-sand px-1.5 text-w-2xs font-semibold tabular-nums text-w-muted";
export const UNREAD = "inline-grid h-[1.125rem] min-w-[1.125rem] shrink-0 place-items-center rounded-[0.3rem] bg-w-alert px-1 text-w-3xs font-bold tabular-nums text-w-onalert";

export const SIDEBAR = "bg-w-sand/50";
export const NAV_HEADING = "mb-1 flex items-center gap-2 px-2.5 text-w-xs font-medium text-w-muted";
export const NAV_ITEM = `relative flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-w-sm transition-colors ${EASE} ${FOCUS} focus-visible:ring-offset-0`;
export const NAV_ON = "bg-w-paper font-semibold text-w-strong ring-2 ring-inset ring-w-line2";
export const NAV_OFF = "nav-off text-w-text2 hover:bg-w-paper/60 hover:text-w-text";
export const NAV_GLIDE = "relative [&_.nav-off:hover]:bg-transparent";
export const NAV_PILL = "rounded-lg bg-w-paper/70";

export const SCRIM = "fixed inset-0 grid place-items-center bg-w-scrim/55 p-3 backdrop-blur-[2px]";
export const MODAL =
  "w-full origin-center overflow-y-auto rounded-xl border-[3px] border-w-ink bg-w-paper text-w-text shadow-[0_6px_0_rgb(var(--w-ink))]";
