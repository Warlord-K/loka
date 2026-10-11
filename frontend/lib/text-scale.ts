export const TEXT_SCALE_KEY = "world.textScale";
export const TEXT_SCALE_EVENT = "world:text-scale";

const STEPS = [0.75, 0.8125, 0.875, 0.9375, 1, 1.0625, 1.125, 1.25, 1.375, 1.5];

export const TEXT_SCALE_SCRIPT = `(function(){try{if(location.pathname.indexOf("/world")!==0)return;var v=parseFloat(localStorage.getItem("${TEXT_SCALE_KEY}")||"");if(v>=${STEPS[0]}&&v<=${STEPS[STEPS.length - 1]})document.documentElement.style.setProperty("--w-text-scale",String(v))}catch(e){}})();`;

export function textScale(): number {
  const v = parseFloat(document.documentElement.style.getPropertyValue("--w-text-scale"));
  return Number.isFinite(v) && v > 0 ? v : 1;
}

export function setTextScale(value: number): number {
  const v = Math.min(STEPS[STEPS.length - 1], Math.max(STEPS[0], value));
  const root = document.documentElement.style;
  if (v === 1) root.removeProperty("--w-text-scale");
  else root.setProperty("--w-text-scale", String(v));
  try {
    if (v === 1) localStorage.removeItem(TEXT_SCALE_KEY);
    else localStorage.setItem(TEXT_SCALE_KEY, String(v));
  } catch {}
  window.dispatchEvent(new CustomEvent(TEXT_SCALE_EVENT, { detail: v }));
  return v;
}

export function stepTextScale(direction: -1 | 0 | 1): number {
  if (direction === 0) return setTextScale(1);
  const now = textScale();
  const next = direction > 0 ? STEPS.find((s) => s > now + 0.001) : [...STEPS].reverse().find((s) => s < now - 0.001);
  return setTextScale(next ?? now);
}

export function installTextScaleKeys(): () => void {
  const onKey = (e: KeyboardEvent) => {
    if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
    const direction = e.key === "=" || e.key === "+" ? 1 : e.key === "-" || e.key === "_" ? -1 : e.key === "0" ? 0 : null;
    if (direction === null) return;
    e.preventDefault();
    stepTextScale(direction);
  };
  const onStorage = (e: StorageEvent) => {
    if (e.key !== TEXT_SCALE_KEY) return;
    const v = parseFloat(e.newValue ?? "");
    setTextScale(Number.isFinite(v) ? v : 1);
  };
  window.addEventListener("keydown", onKey, true);
  window.addEventListener("storage", onStorage);
  return () => {
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("storage", onStorage);
  };
}
