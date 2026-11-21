export type SendFailure = {
  retry: boolean;
  retrying: string;
  reason: string;
};

export const SEND_RETRY_DELAYS = [1000, 2000, 4000, 8000, 15000];

const DISK_FULL = /no space left|disk (is )?full|enospc|could not extend file|insufficient storage/i;

function statusOf(err: unknown): number | null {
  if (!err || typeof err !== "object") return null;
  const status = (err as { status?: unknown }).status;
  if (typeof status === "number" && Number.isFinite(status)) return status;
  const message = (err as { message?: unknown }).message;
  if (typeof message !== "string") return null;
  const lead = /^(\d{3})\b/.exec(message.trim());
  return lead ? Number(lead[1]) : null;
}

function serverReason(err: unknown, status: number | null): string | null {
  const message = err instanceof Error ? err.message.trim() : "";
  if (!message || /^\d{3}\b/.test(message)) return null;
  if (status === null) return null;
  return /[.!?]$/.test(message) ? message : `${message}.`;
}

function stopped(reason: string): SendFailure {
  return { retry: false, retrying: reason, reason };
}

function transient(retrying: string, reason: string): SendFailure {
  return { retry: true, retrying, reason };
}

export function sendFailure(err: unknown): SendFailure {
  if (err instanceof TypeError) return transient("Can't reach Loka, retrying...", "Loka is not reachable.");
  const status = statusOf(err);
  const text = err instanceof Error ? err.message : "";
  if (status === 507 || DISK_FULL.test(text)) return stopped("The server is out of disk space.");
  const said = serverReason(err, status);
  switch (status) {
    case null:
      return stopped("Something went wrong.");
    case 408:
      return transient("Loka is slow to answer, retrying...", "Loka took too long to answer.");
    case 429:
      return stopped("You are sending messages too fast. Wait a minute.");
    case 401:
      return stopped("You are signed out. Sign in again.");
    case 403:
      return stopped(said ?? "You can't message this agent right now.");
    case 404:
      return stopped(said ?? "This agent or thread is gone.");
    case 409:
      return stopped(said ?? "This agent can't take messages right now.");
    case 413:
      return stopped("Too large to send. Remove an attachment or shorten it.");
    case 400:
    case 422:
      return stopped(said ?? "Loka could not read this message.");
  }
  if (status >= 500) return transient("Server restarting, retrying...", "The server is not answering.");
  return stopped(said ?? `Loka answered with error ${status}.`);
}

export function notSent(failure: SendFailure, what = "message"): string {
  return `Not sent. ${failure.reason} Your ${what} is back in the box, press Send to try again.`;
}
