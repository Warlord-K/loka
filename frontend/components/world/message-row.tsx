"use client";

import type { ReactNode } from "react";
import { ChatsCircle, Quotes } from "@phosphor-icons/react";
import { cn } from "@/lib/utils";
import { messageTime } from "@/lib/world";
import { CopyButton, DeleteButton, EditButton, MessageStamp, RewindButton } from "./chat-kit";
import { HIGHLIGHT, LinkButton } from "./message-links";

export function ReplyButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="Reply in thread"
      title="Reply in thread"
      className="rounded-md p-0.5 text-w-muted transition-colors hover:bg-w-sand hover:text-w-strong"
    >
      <ChatsCircle size={12} weight="bold" />
    </button>
  );
}

export function QuoteButton({ onClick, label = "Quote in chat" }: { onClick: () => void; label?: string }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className="rounded-md p-0.5 text-w-muted transition-colors hover:bg-w-sand hover:text-w-strong"
    >
      <Quotes size={12} weight="bold" />
    </button>
  );
}

export interface MessageActions {
  copy?: string;
  link?: string;
  onQuote?: () => void;
  onReply?: () => void;
  onEdit?: () => void;
  onRewind?: () => void;
  onDelete?: () => void;
}

export function MessageToolbar({ actions, className }: { actions: MessageActions; className?: string }) {
  const { copy, link, onQuote, onReply, onEdit, onRewind, onDelete } = actions;
  if (!copy?.trim() && !link && !onQuote && !onReply && !onEdit && !onRewind && !onDelete) return null;
  return (
    <div
      className={cn(
        "pointer-events-none absolute -top-4 right-4 z-10 flex origin-bottom-right translate-y-1 scale-95 items-center gap-0.5 rounded-lg border-2 border-w-line2 bg-w-paper p-0.5 opacity-0 shadow-[0_4px_12px_rgb(var(--w-ink)/0.18)] transition-[opacity,transform] duration-150 [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] group-focus-within:pointer-events-auto group-focus-within:translate-y-0 group-focus-within:scale-100 group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:translate-y-0 group-hover:scale-100 group-hover:opacity-100 motion-reduce:transform-none",
        "[&>button]:grid [&>button]:h-7 [&>button]:w-7 [&>button]:place-items-center [&>button]:p-0 [&>button]:opacity-100 [&>button]:transition-colors [&_svg]:h-[0.9375rem] [&_svg]:w-[0.9375rem]",
        className,
      )}
    >
      {onReply && <ReplyButton onClick={onReply} />}
      {onQuote && <QuoteButton onClick={onQuote} />}
      {copy !== undefined && <CopyButton text={copy} />}
      {link && <LinkButton reference={link} />}
      {onEdit && <EditButton onClick={onEdit} />}
      {onRewind && <RewindButton onClick={onRewind} />}
      {onDelete && <DeleteButton onClick={onDelete} />}
    </div>
  );
}

export function MessageAuthor({
  name,
  onOpen,
  agent,
  you,
  createdAt,
  editedAt,
  badges,
  children,
}: {
  name: string;
  onOpen?: () => void;
  agent?: boolean;
  you?: boolean;
  createdAt: string;
  editedAt?: string | null;
  badges?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <p className="flex flex-wrap items-baseline gap-x-1.5 text-w-sm">
      {onOpen ? (
        <button type="button" onClick={onOpen} className="font-semibold text-w-strong transition-colors hover:text-w-link">
          {name}
        </button>
      ) : (
        <span className="font-semibold text-w-strong">{name}</span>
      )}
      {agent && <span className="rounded bg-w-sand px-1 py-px text-w-3xs font-semibold text-w-muted">Agent</span>}
      {you && <span className="text-w-2xs text-w-faint">you</span>}
      {badges}
      <MessageStamp createdAt={createdAt} editedAt={editedAt} />
      {children}
    </p>
  );
}

export function MessageRow({
  id,
  first = true,
  createdAt,
  avatar,
  header,
  actions,
  editing,
  highlighted,
  flat,
  hover = true,
  className,
  children,
}: {
  id?: string;
  first?: boolean;
  createdAt: string;
  avatar: ReactNode;
  header?: ReactNode;
  actions?: MessageActions;
  editing?: boolean;
  highlighted?: boolean;
  flat?: boolean;
  hover?: boolean;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      data-message-id={id}
      className={cn(
        "group relative flex select-text gap-3 px-4 text-w-base leading-relaxed transition-colors",
        hover && "hover:bg-w-sand/50",
        flat ? "py-1.5" : first ? "mt-3 pb-1 pt-1.5" : "py-0.5",
        highlighted && HIGHLIGHT,
        className,
      )}
    >
      <div className="w-9 shrink-0">
        {first ? (
          avatar
        ) : (
          <time dateTime={createdAt} className="invisible block pt-1 text-right text-w-3xs tabular-nums text-w-faint group-hover:visible">
            {messageTime(createdAt)}
          </time>
        )}
      </div>
      <div className="min-w-0 flex-1 pb-0.5">
        {first && header}
        {children}
      </div>
      {!editing && actions && <MessageToolbar actions={actions} />}
    </div>
  );
}
