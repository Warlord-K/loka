export function pollWhileVisible(load: () => void, everyMs: number): () => void {
  let last = Date.now();
  const run = () => {
    last = Date.now();
    load();
  };
  const tick = () => {
    if (document.visibilityState === "visible" && Date.now() - last >= everyMs - 1000) run();
  };
  const onVisible = () => {
    if (document.visibilityState === "visible" && Date.now() - last >= Math.min(everyMs, 10_000)) run();
  };
  const id = window.setInterval(tick, everyMs);
  document.addEventListener("visibilitychange", onVisible);
  window.addEventListener("focus", onVisible);
  return () => {
    window.clearInterval(id);
    document.removeEventListener("visibilitychange", onVisible);
    window.removeEventListener("focus", onVisible);
  };
}
