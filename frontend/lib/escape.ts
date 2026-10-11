import { isDesktopApp } from "./desktop";

const taken = new WeakSet<Event>();

export function takeEscape(e: { preventDefault: () => void; nativeEvent?: Event } | Event): void {
  e.preventDefault();
  taken.add("nativeEvent" in e && e.nativeEvent ? e.nativeEvent : (e as Event));
}

export function escapeTaken(e: KeyboardEvent): boolean {
  if (taken.has(e)) return true;
  if (!isDesktopApp()) return e.defaultPrevented;
  return document.querySelector("[data-radix-popper-content-wrapper]") !== null;
}
