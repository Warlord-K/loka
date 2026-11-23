"use client";

import { useMemo, type MutableRefObject } from "react";
import type { MessageQuote } from "../quote";
import { TerminalPanel, type TerminalReader, type TerminalSource } from "../terminal";

export function TerminalPane({
  sessionKey,
  onHide,
  onError,
  readerRef,
  request = null,
  onRequestTaken,
  onQuote,
}: {
  sessionKey: string;
  onHide: () => void;
  onError: (message: string) => void;
  readerRef?: MutableRefObject<TerminalReader | null>;
  request?: { id: number; command: string } | null;
  onRequestTaken?: (id: number) => void;
  onQuote?: (quote: MessageQuote) => void;
}) {
  const source = useMemo<TerminalSource>(() => ({ kind: "workshop", sessionKey }), [sessionKey]);
  const asked = useMemo(() => (request ? { id: request.id, command: request.command, runnerId: null } : null), [request]);
  return (
    <TerminalPanel
      meId={null}
      source={source}
      onClose={onHide}
      readerRef={readerRef}
      request={asked}
      onTyped={(id) => onRequestTaken?.(id)}
      onQuote={onQuote ?? null}
      onNote={onError}
    />
  );
}
