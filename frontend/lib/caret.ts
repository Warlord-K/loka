export type FocusPoint = { x: number; y: number; before: string; after: string };

export function focusPointAt(x: number, y: number): FocusPoint {
  const doc = document as Document & { caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null };
  let node: Node | null = null;
  let offset = 0;
  if (doc.caretPositionFromPoint) {
    const at = doc.caretPositionFromPoint(x, y);
    node = at?.offsetNode ?? null;
    offset = at?.offset ?? 0;
  } else if (document.caretRangeFromPoint) {
    const range = document.caretRangeFromPoint(x, y);
    node = range?.startContainer ?? null;
    offset = range?.startOffset ?? 0;
  }
  const text = node?.nodeType === 3 ? node.textContent ?? "" : "";
  return { x, y, before: text.slice(Math.max(0, offset - 40), offset), after: text.slice(offset, offset + 40) };
}
