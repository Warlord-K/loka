export const SCROLL_JUMP = "loka:scroll-jump";

type Block = "start" | "center" | "nearest" | "end";

function scrollArea(el: Element): HTMLElement | null {
  let node = el.parentElement;
  while (node) {
    const { overflowY } = getComputedStyle(node);
    if ((overflowY === "auto" || overflowY === "scroll") && node.scrollHeight > node.clientHeight) return node;
    node = node.parentElement;
  }
  return null;
}

export function scrollWithin(el: Element | null | undefined, opts: { block?: Block; smooth?: boolean } = {}) {
  if (!el) return;
  const area = scrollArea(el);
  if (!area) return;
  const box = area.getBoundingClientRect();
  const rect = el.getBoundingClientRect();
  const delta = rect.top - box.top - area.clientTop;
  const view = area.clientHeight;
  const block = opts.block ?? "start";
  let top: number;
  const margin = parseFloat(getComputedStyle(el).scrollMarginTop) || 0;
  if (block === "start") top = area.scrollTop + delta - margin;
  else if (block === "end") top = area.scrollTop + delta + rect.height - view;
  else if (block === "center") top = area.scrollTop + delta - (view - rect.height) / 2;
  else if (delta < 0) top = area.scrollTop + delta;
  else if (delta + rect.height > view) top = area.scrollTop + delta + rect.height - view;
  else return;
  area.dispatchEvent(new Event(SCROLL_JUMP));
  const calm = typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  area.scrollTo({ top: Math.max(0, top), behavior: opts.smooth && !calm ? "smooth" : "auto" });
}

export function pinClippedScroll(root: HTMLElement): () => void {
  const reset = (event: Event) => {
    const node = event.target;
    if (!(node instanceof HTMLElement) || !root.contains(node)) return;
    const style = getComputedStyle(node);
    if (node.scrollTop && (style.overflowY === "hidden" || style.overflowY === "clip")) node.scrollTop = 0;
    if (node.scrollLeft && (style.overflowX === "hidden" || style.overflowX === "clip")) node.scrollLeft = 0;
  };
  document.addEventListener("scroll", reset, true);
  return () => document.removeEventListener("scroll", reset, true);
}
