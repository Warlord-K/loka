export const BRACKET_WAIT_MS = 3000;
export const OUTPUT_QUIET_MS = 400;

const PASTE_START = "\x1b[200~";
const PASTE_END = "\x1b[201~";

export type Typing = { kind: "type"; data: string } | { kind: "copy"; text: string } | { kind: "empty" };

export function cleanCommand(command: string): string {
  return command
    .replace(/\r\n?/g, "\n")
    .replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .replace(/^\n+|\n+$/g, "");
}

export function typingFor(command: string, bracketed: boolean): Typing {
  const clean = cleanCommand(command);
  if (!clean.trim()) return { kind: "empty" };
  if (bracketed) return { kind: "type", data: `${PASTE_START}${clean}${PASTE_END}` };
  if (clean.includes("\n")) return { kind: "copy", text: clean };
  return { kind: "type", data: clean.replace(/\t/g, " ") };
}

export function readyToType(at: { bracketed: boolean; sinceLive: number; sinceOutput: number }): boolean {
  if (at.bracketed) return true;
  if (at.sinceLive < BRACKET_WAIT_MS) return false;
  return at.sinceOutput >= OUTPUT_QUIET_MS || at.sinceLive >= BRACKET_WAIT_MS * 2;
}

export type Typed = "typed" | "copied" | "not_copied" | "empty" | "cancelled";

export type ShellPhase = "idle" | "starting" | "connecting" | "live" | "ended" | "error";

export type TypePlan = "type" | "queue" | "ask" | "open" | "none";

function opening(phase: ShellPhase): boolean {
  return phase === "starting" || phase === "connecting" || phase === "live";
}

export function planRequest(at: { phase: ShellPhase; current: string | null; target: string | null }): TypePlan {
  if (!at.target) return "none";
  if (!opening(at.phase)) return "open";
  if (at.current !== at.target) return "ask";
  return at.phase === "live" ? "type" : "queue";
}

export function planSwitch(at: { go: boolean; phase: ShellPhase; current: string | null; target: string }): { plan: TypePlan; device: string | null } {
  if (at.go) return { plan: "open", device: at.target };
  if (!at.current) return { plan: "none", device: null };
  if (!opening(at.phase)) return { plan: "open", device: at.current };
  return { plan: at.phase === "live" ? "type" : "queue", device: at.current };
}

export type TypedReport = { result: Typed; device: string; note: string; context: string };

export function typedReport(result: Typed, device: string): TypedReport | null {
  if (result === "typed") return { result, device, note: `Typed at the prompt on ${device}. Nothing runs until you press Enter.`, context: `Typed at the prompt in the terminal on ${device}, not run yet.` };
  if (result === "copied")
    return {
      result,
      device,
      note: `The shell on ${device} does not take pasted lines, so the command was copied instead. Paste it at the prompt.`,
      context: `Copied to the clipboard to paste into the terminal on ${device}, not typed or run yet.`,
    };
  if (result === "not_copied")
    return {
      result,
      device,
      note: `The shell on ${device} does not take pasted lines. Copy the command from the chat and paste it at the prompt.`,
      context: `Not typed into the terminal on ${device}: that shell does not take pasted lines and it could not be copied, so it has not run.`,
    };
  return null;
}
