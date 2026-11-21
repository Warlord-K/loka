export interface DotenvEntry {
  name: string;
  value: string;
  line: number;
  problem: string | null;
  duplicate: boolean;
}

export interface DotenvResult {
  entries: DotenvEntry[];
  skipped: { line: number; reason: string }[];
}

export const MAX_NAME_BYTES = 128;
export const MAX_VALUE_BYTES = 64 * 1024;
export const MAX_IMPORT_ITEMS = 500;
export const MAX_IMPORT_BYTES = 8 * 1024 * 1024;

const RESERVED_PREFIXES = ["LOKA_", "KARNA_", "WORLD_"];

const DENIED_NAMES = new Set([
  "PATH", "HOME", "SHELL", "USER", "LOGNAME", "PWD", "OLDPWD", "TMPDIR", "TMP", "TEMP", "TERM", "ENV",
  "BASH_ENV", "BASHOPTS", "SHELLOPTS", "CDPATH", "GLOBIGNORE", "IFS", "PROMPT_COMMAND", "PS4", "ZDOTDIR",
  "CODEX_HOME", "CLAUDE_CONFIG_DIR", "NODE_OPTIONS", "NODE_PATH", "PYTHONPATH", "PYTHONHOME", "PYTHONSTARTUP",
  "RUBYOPT", "RUBYLIB", "PERL5OPT", "PERL5LIB", "JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "_JAVA_OPTIONS",
  "RUSTC_WRAPPER", "RUSTDOCFLAGS", "REDIS_URL", "CONFIG_PATH",
]);

const DENIED_PREFIXES = ["LD_", "DYLD_", "GIT_"];

const bytes = (text: string) => new TextEncoder().encode(text).length;

export function nameProblem(name: string): string | null {
  if (!name) return "The name is empty.";
  if (bytes(name) > MAX_NAME_BYTES) return "Names can be at most 128 characters.";
  if (!/^[A-Z_][A-Z0-9_]*$/.test(name)) {
    return /^[A-Za-z_][A-Za-z0-9_]*$/.test(name)
      ? "Use capital letters, digits and underscores."
      : "Names start with a letter or underscore and use only capital letters, digits and underscores.";
  }
  if (RESERVED_PREFIXES.some((prefix) => name.startsWith(prefix))) return "Names starting with LOKA_, KARNA_ or WORLD_ are reserved.";
  if (DENIED_NAMES.has(name) || DENIED_PREFIXES.some((prefix) => name.startsWith(prefix))) {
    return "Loka sets this variable itself, so it cannot be saved.";
  }
  return null;
}

export function valueProblem(value: string): string | null {
  if (!value) return "The value is empty.";
  if (value.includes("\0")) return "The value contains a null character.";
  if (bytes(value) > MAX_VALUE_BYTES) return "Values can be at most 64 KB.";
  return null;
}

export function importProblem(items: { name: string; value: string }[]): string | null {
  if (items.length > MAX_IMPORT_ITEMS) return `Too many keys at once. Save at most ${MAX_IMPORT_ITEMS} in one import.`;
  if (bytes(JSON.stringify({ items })) > MAX_IMPORT_BYTES) return "This import is too large. Keep it under 8 MB, or split it into smaller imports.";
  return null;
}

const ESCAPES: Record<string, string> = { n: "\n", r: "\r", t: "\t", '"': '"', "\\": "\\", $: "$" };

function readQuoted(source: string, start: number, quote: string): { value: string; end: number } | null {
  let value = "";
  let i = start;
  while (i < source.length) {
    const ch = source[i];
    if (ch === quote) return { value, end: i + 1 };
    if (quote === '"' && ch === "\\" && i + 1 < source.length) {
      const next = source[i + 1];
      value += ESCAPES[next] ?? `\\${next}`;
      i += 2;
      continue;
    }
    value += ch;
    i += 1;
  }
  return null;
}

export function parseDotenv(text: string): DotenvResult {
  const source = text.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  const raw: Omit<DotenvEntry, "problem" | "duplicate">[] = [];
  const skipped: DotenvResult["skipped"] = [];
  const starts = [0];
  for (let i = 0; i < source.length; i += 1) if (source[i] === "\n") starts.push(i + 1);
  const lineOf = (index: number) => {
    let low = 0;
    let high = starts.length - 1;
    while (low < high) {
      const mid = (low + high + 1) >> 1;
      if (starts[mid] <= index) low = mid;
      else high = mid - 1;
    }
    return low + 1;
  };
  const endOfLine = (index: number) => {
    const next = source.indexOf("\n", index);
    return next === -1 ? source.length : next;
  };

  let pos = 0;
  while (pos < source.length) {
    const end = endOfLine(pos);
    const current = source.slice(pos, end);
    const trimmed = current.trim();
    const line = lineOf(pos);

    if (!trimmed || trimmed.startsWith("#")) {
      pos = end + 1;
      continue;
    }

    const match = /^\s*(?:export\s+)?([^=\s]+)\s*=/.exec(current);
    if (!match) {
      skipped.push({ line, reason: "Not a NAME=value line." });
      pos = end + 1;
      continue;
    }

    const name = match[1];
    let cursor = pos + match[0].length;
    while (cursor < end && (source[cursor] === " " || source[cursor] === "\t")) cursor += 1;
    const quote = source[cursor];

    if (quote === '"' || quote === "'" || quote === "`") {
      const quoted = readQuoted(source, cursor + 1, quote);
      if (!quoted) {
        skipped.push({ line, reason: "The quote is never closed." });
        pos = end + 1;
        continue;
      }
      raw.push({ name, value: quoted.value, line });
      const stop = endOfLine(quoted.end);
      const rest = source.slice(quoted.end, stop).trim();
      if (rest && !rest.startsWith("#")) skipped.push({ line: lineOf(quoted.end), reason: "Text after the closing quote was ignored." });
      pos = stop + 1;
      continue;
    }

    let value = source.slice(cursor, end);
    const comment = /\s#/.exec(value);
    if (comment) value = value.slice(0, comment.index);
    raw.push({ name, value: value.trim(), line });
    pos = end + 1;
  }

  const lastIndex = new Map<string, number>();
  raw.forEach((entry, index) => lastIndex.set(entry.name, index));
  const counts = new Map<string, number>();
  raw.forEach((entry) => counts.set(entry.name, (counts.get(entry.name) ?? 0) + 1));

  const entries = raw
    .filter((entry, index) => lastIndex.get(entry.name) === index)
    .map((entry) => ({
      ...entry,
      problem: nameProblem(entry.name) ?? valueProblem(entry.value),
      duplicate: (counts.get(entry.name) ?? 0) > 1,
    }));

  return { entries, skipped };
}
