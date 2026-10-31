const CLI_NAMES: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  omp: "omp",
  cursor: "Cursor",
  opencode: "OpenCode",
};

const CLAUDE_ALIASES = new Set(["sonnet", "opus", "haiku", "fable"]);
const UPPER = new Set(["gpt", "glm", "ai", "api", "xl"]);

export function cliName(cli: string | null | undefined): string {
  if (!cli) return "";
  return CLI_NAMES[cli] ?? cli.charAt(0).toUpperCase() + cli.slice(1);
}

function word(part: string): string {
  if (!part) return part;
  if (UPPER.has(part.toLowerCase())) return part.toUpperCase();
  if (/^[a-z]\d/.test(part)) return part.toUpperCase();
  return part.charAt(0).toUpperCase() + part.slice(1);
}

export function modelName(model: string | null | undefined): string {
  if (!model) return "";
  const bare = model.includes("/") ? model.slice(model.lastIndexOf("/") + 1) : model;
  const gpt = bare.match(/^gpt-([\d.]+[a-z]?)(?:-(.+))?$/i);
  if (gpt) return `GPT-${gpt[1]}${gpt[2] ? ` ${gpt[2].split(/[-_]/).map(word).join(" ")}` : ""}`;
  const parts = bare.split(/[-_\s]+/).filter(Boolean);
  const out: string[] = [];
  for (const part of parts) {
    const prev = out[out.length - 1];
    if (/^\d+$/.test(part) && prev && /\d$/.test(prev)) out[out.length - 1] = `${prev}.${part}`;
    else out.push(/^\d/.test(part) ? part : word(part));
  }
  return out.join(" ");
}

export function runtimeName(cli: string | null | undefined, model: string | null | undefined): string {
  const m = (model ?? "").toLowerCase();
  if (cli === "claude" && CLAUDE_ALIASES.has(m)) return `Claude ${modelName(model)}`;
  if (m.startsWith("claude")) return modelName(model);
  if (!model) return cliName(cli);
  if (!cli) return modelName(model);
  return `${cliName(cli)} · ${modelName(model)}`;
}
