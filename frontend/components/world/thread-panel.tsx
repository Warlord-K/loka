"use client";

import { Fragment, useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { ChatsCircle } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import type { Attachment, Person, ReplyAuthor, WorldAgent, WorldEvent } from "@/lib/world";
import { AgentFace, PersonAvatar } from "./channel-ui";
import { RelativeTime } from "./relative-time";
import { AttachButton, AttachmentGrid, MessageEditor, NewMessagesPill, PendingStrip, RichText, useAttachments, useFollowBottom } from "./chat-kit";
import { MessageAuthor, MessageRow } from "./message-row";
export { ReplyButton } from "./message-row";
import { useDraft } from "./drafts";
import { useMentionPicker, type MentionCandidate } from "./mention-picker";
import { CloseButton } from "./settings-shell";
import { CHUNKY } from "./styles";
import { SpokenMark, VoiceField } from "./voice-ui";
import { Shimmer } from "./work-ui";
import { MemoryOffer } from "./quick-chat";
import { takeEscape } from "@/lib/escape";
import { notSent, sendFailure } from "@/lib/send-failure";

export interface ThreadItem {
  key: string;
  name: string;
  avatar: ReactNode;
  body: string;
  createdAt: string;
  editedAt?: string | null;
  attachments?: Attachment[];
  agent?: boolean;
  spoken?: boolean;
  call?: boolean;
  onEdit?: (body: string) => Promise<void>;
  onDelete?: () => Promise<void>;
  memory?: { line: string; answered: string | null; onAnswer: (choice: string) => Promise<void> };
}

const THINKING_WINDOW_MS = 30 * 60 * 1000;

const NO_CANDIDATES: MentionCandidate[] = [];

export function RepliesButton({
  count,
  lastAt,
  authors,
  agents = [],
  onOpen,
}: {
  count: number;
  lastAt?: string | null;
  authors?: ReplyAuthor[] | null;
  agents?: WorldAgent[];
  onOpen: () => void;
}) {
  if (!count) return null;
  const faces = (authors ?? []).slice(0, 4);
  const names = faces.map((a) => a.name).filter(Boolean).join(", ");
  return (
    <button
      type="button"
      onClick={onOpen}
      title={names ? `Replies from ${names}` : undefined}
      className="mt-1 inline-flex items-center gap-1.5 rounded-md px-1.5 py-0.5 text-w-xs font-semibold text-w-link transition-colors hover:bg-w-aqua"
    >
      {faces.length > 0 ? (
        <span className="flex -space-x-1">
          {faces.map((a) => {
            const agent = a.agent_id ? agents.find((x) => x.id === a.agent_id) : undefined;
            const key = `${a.kind}:${a.agent_id ?? a.user_id ?? a.name}`;
            return agent ? (
              <AgentFace key={key} agent={agent} size={18} className="ring-2 ring-w-paper" />
            ) : (
              <PersonAvatar key={key} id={a.user_id ?? a.agent_id ?? key} name={a.name ?? "?"} size={18} soft className="ring-2 ring-w-paper" />
            );
          })}
        </span>
      ) : (
        <ChatsCircle size={14} weight="bold" />
      )}
      {count === 1 ? "1 reply" : `${count} replies`}
      {lastAt && (
        <RelativeTime at={lastAt} prefix="last reply " className="whitespace-nowrap text-w-2xs font-normal text-w-muted" />
      )}
    </button>
  );
}

export function ThreadPanel({
  title,
  subtitle,
  draftKey,
  load,
  refreshOn,
  send,
  canPost,
  handles,
  people,
  onOpenAgent,
  onOpenPerson,
  candidates = NO_CANDIDATES,
  onClose,
  className,
  style,
  waitingFor,
  note,
  typing,
}: {
  waitingFor?: string;
  note?: ReactNode;
  typing?: ReactNode;
  title: string;
  subtitle?: string;
  draftKey: string;
  load: () => Promise<ThreadItem[]>;
  refreshOn: (ev: WorldEvent) => boolean;
  send: (text: string, spoken: boolean, attachments: string[]) => Promise<void>;
  canPost: boolean;
  handles: Map<string, WorldAgent>;
  people?: Map<string, Person>;
  onOpenAgent: (id: string) => void;
  onOpenPerson?: (id: string) => void;
  candidates?: MentionCandidate[];
  onClose: () => void;
  className?: string;
  style?: React.CSSProperties;
}) {
  const [items, setItems] = useState<ThreadItem[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useDraft(draftKey);
  const inputRef = useRef<HTMLTextAreaElement | HTMLInputElement | null>(null);
  const endRef = useRef<HTMLDivElement>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  const refreshRef = useRef(refreshOn);
  refreshRef.current = refreshOn;

  const refresh = useCallback(() => {
    void loadRef
      .current()
      .then((next) => {
        setItems(next);
        setFailed(false);
      })
      .catch(() => setFailed(true));
  }, []);

  useEffect(() => {
    setItems(null);
    refresh();
  }, [draftKey, refresh]);

  useEffect(() => {
    const onEvent = (e: Event) => {
      if (refreshRef.current((e as CustomEvent<WorldEvent>).detail)) refresh();
    };
    window.addEventListener("world-event", onEvent);
    return () => window.removeEventListener("world-event", onEvent);
  }, [refresh]);

  const follow = useFollowBottom(endRef, items?.[items.length - 1]?.key, draftKey);

  useEffect(() => {
    if (!window.matchMedia("(pointer: fine)").matches) return;
    const id = window.setTimeout(() => inputRef.current?.focus({ preventScroll: true }), 60);
    return () => window.clearTimeout(id);
  }, [draftKey]);

  const picker = useMentionPicker({ value: draft, onChange: setDraft, inputRef, candidates });
  const files = useAttachments();
  const [trash, setTrash] = useState<ThreadItem | null>(null);
  const trashRef = useRef(trash);
  trashRef.current = trash;
  const commitDelete = useCallback(
    (item: ThreadItem) => {
      void item
        .onDelete?.()
        .then(() => refresh())
        .catch(() => setError("Could not delete that reply. Try again."));
    },
    [refresh],
  );
  useEffect(() => {
    if (!trash) return;
    const t = window.setTimeout(() => {
      commitDelete(trash);
      setTrash(null);
    }, 5000);
    return () => window.clearTimeout(t);
  }, [trash, commitDelete]);
  useEffect(
    () => () => {
      if (trashRef.current) commitDelete(trashRef.current);
    },
    [commitDelete],
  );
  const remove = (item: ThreadItem) => {
    if (trashRef.current) commitDelete(trashRef.current);
    setTrash(item);
  };

  const [dictated, setDictated] = useState(false);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const text = draft.trim();
    const attachments = files.ready;
    if ((!text && !attachments.length) || sending || files.uploading) return;
    setSending(true);
    setError(null);
    setDraft("");
    follow.stick();
    try {
      await send(text, dictated, attachments);
      files.clear();
      setDictated(false);
      refresh();
      follow.stick();
    } catch (err) {
      setDraft(text);
      setError(notSent(sendFailure(err), "reply"));
    } finally {
      setSending(false);
    }
  };

  const [root, ...allReplies] = items ?? [];
  const replies = trash ? allReplies.filter((r) => r.key !== trash.key) : allReplies;
  const lastReply = replies[replies.length - 1];
  const thinking = !!waitingFor && !!lastReply && !lastReply.agent && Date.now() - new Date(lastReply.createdAt).getTime() < THINKING_WINDOW_MS;

  return (
    <aside
      aria-label="Thread"
      onKeyDown={(e) => {
        if (e.key !== "Escape" || e.defaultPrevented) return;
        takeEscape(e);
        onClose();
      }}
      style={style}
      className={cn("flex min-h-0 flex-col bg-w-paper animate-[fade-in_160ms_ease-out] motion-reduce:animate-none", className)}
    >
      <header className="flex shrink-0 items-center gap-2 border-b-2 border-dashed border-w-line2 px-4 py-3">
        <ChatsCircle size={18} weight="bold" className="shrink-0 text-w-link" />
        <div className="min-w-0 flex-1">
          <p className="truncate text-w-md font-semibold text-w-strong">{title}</p>
          {subtitle && <p className="truncate text-w-xs text-w-muted">{subtitle}</p>}
        </div>
        <CloseButton onClose={onClose} label="Close thread" />
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto pb-2 pt-3">
        {items === null ? (
          <p className="px-4 text-w-sm text-w-muted">{failed ? "Could not load this thread." : <Shimmer>Loading the thread</Shimmer>}</p>
        ) : !root ? (
          <p className="px-4 text-w-sm text-w-muted">This message is gone.</p>
        ) : (
          <>
            <Row item={root} handles={handles} people={people} onOpenAgent={onOpenAgent} onOpenPerson={onOpenPerson} onSaved={refresh} />
            <div className="my-2 flex items-center gap-2 px-4 text-w-xs text-w-muted">
              <span>{replies.length === 0 ? "No replies yet" : replies.length === 1 ? "1 reply" : `${replies.length} replies`}</span>
              <span className="h-0 flex-1 border-t border-dashed border-w-line2" />
            </div>
            {replies.map((item) => (
              <Fragment key={item.key}>
                <Row item={item} handles={handles} people={people} onOpenAgent={onOpenAgent} onOpenPerson={onOpenPerson} onRemove={item.onDelete ? () => remove(item) : undefined} onSaved={refresh} />
              </Fragment>
            ))}
            {thinking && (
              <p className="px-4 py-2 pl-[4.25rem] text-w-sm">
                <Shimmer>{`${waitingFor} is thinking`}</Shimmer>
              </p>
            )}
            {note && <div className="mx-4 mt-2">{note}</div>}
          </>
        )}
        <NewMessagesPill show={follow.unseen} away={follow.away} onClick={follow.jump} />
        <div ref={endRef} />
      </div>
      {error && <p role="alert" className="mx-4 mb-2 rounded-lg border-2 border-w-danger bg-w-rose p-2 text-w-sm text-w-danger">{error}</p>}
      {trash && (
        <div role="status" className="mx-4 mb-2 flex items-center gap-3 rounded-lg border-2 border-w-line2 bg-w-sand px-3 py-1.5 text-w-sm text-w-text2">
          <span className="min-w-0 flex-1">Reply deleted.</span>
          <button type="button" onClick={() => setTrash(null)} className="font-semibold text-w-link hover:underline">
            Undo
          </button>
        </div>
      )}
      {canPost && (
        <div className="shrink-0 px-3 pb-3 pt-1" {...files.dropProps}>
          {typing}
          <div className="relative rounded-xl border-2 border-w-line2 bg-w-card transition-colors focus-within:border-w-teal focus-within:ring-2 focus-within:ring-w-teal/25">
            {picker.popover}
            <PendingStrip pending={files.pending} onRemove={files.remove} />
            <form onSubmit={submit} className="flex items-end gap-2 p-2">
              <AttachButton onFiles={files.add} />
              <VoiceField
                composer
                inputRef={inputRef}
                value={draft}
                onChange={(v) => {
                  setDraft(v);
                  if (!v) setDictated(false);
                }}
                onDictated={() => setDictated(true)}
                onKey={picker.onKey}
                placeholder="Reply in thread"
                ariaLabel="Reply"
                className="[&_textarea]:border-0 [&_textarea]:bg-transparent [&_textarea]:!shadow-none [&_textarea]:!ring-0"
              />
              <button
                type="submit"
                disabled={sending || files.uploading || (!draft.trim() && !files.ready.length)}
                className={cn(CHUNKY, "h-10 bg-w-teal px-3 text-w-base font-semibold text-w-strong hover:bg-w-teal2")}
              >
                Reply
              </button>
            </form>
          </div>
        </div>
      )}
    </aside>
  );
}

function Row({
  item,
  handles,
  people,
  onOpenAgent,
  onOpenPerson,
  onRemove,
  onSaved,
}: {
  item: ThreadItem;
  handles: Map<string, WorldAgent>;
  people?: Map<string, Person>;
  onOpenAgent: (id: string) => void;
  onOpenPerson?: (id: string) => void;
  onRemove?: () => void;
  onSaved?: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [answering, setAnswering] = useState(false);
  const memory = item.memory;
  return (
    <MessageRow
      flat
      createdAt={item.createdAt}
      avatar={item.avatar}
      editing={editing}
      header={
        <MessageAuthor name={item.name} agent={item.agent} createdAt={item.createdAt} editedAt={item.editedAt}>
          {(item.spoken || item.call) && <SpokenMark call={item.call} />}
        </MessageAuthor>
      }
      actions={{ copy: item.body, onEdit: item.onEdit && item.body ? () => setEditing(true) : undefined, onDelete: onRemove }}
    >
      {editing && item.onEdit ? (
        <MessageEditor
          initial={item.body}
          onSave={async (text) => {
            await item.onEdit?.(text);
            setEditing(false);
            onSaved?.();
          }}
          onCancel={() => setEditing(false)}
        />
      ) : memory ? (
        <MemoryOffer
          line={memory.line}
          answered={memory.answered}
          busy={answering}
          onAnswer={(choice) => {
            setAnswering(true);
            void memory
              .onAnswer(choice)
              .then(() => onSaved?.())
              .finally(() => setAnswering(false));
          }}
        />
      ) : (
        <RichText text={item.body} handles={handles} people={people} onOpenAgent={onOpenAgent} onOpenPerson={onOpenPerson} className="select-text text-w-text" />
      )}
      <AttachmentGrid items={item.attachments ?? []} />
    </MessageRow>
  );
}
