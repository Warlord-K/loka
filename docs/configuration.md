# Configuration

The Mac app sets everything up itself, so most people never need this page. It covers where the app keeps its data, how saved keys work, and the settings you can change when you run the pieces by hand for development.

## Your data

Everything the app keeps lives in `~/Library/Application Support/dev.ygpt.loka`:

- the Postgres data folder (agents, sessions, quests, reports, saved keys)
- `local.json`, with the generated `AUTH_SECRET` and `WORLD_SECRETS_KEY` (saved keys cannot be read without it)
- `config.yaml`, optional, see below
- logs, and the runner's workspaces and worktrees

To back up, quit Loka and copy the whole folder. To restore, quit Loka, put the folder back and open Loka again.

Settings the app reads from your login shell (CLI tokens, `WORLD_BROWSER_LOCAL` and similar) are picked up when the app starts, so restart Loka after changing your shell profile.

## Saved keys

Keys are entered under **Settings > Environment**, outside agent chat. The app lists only each name, when it changed and which agents may use it; values are never shown again. Grant a key to an agent, then tell the agent to use `${secret:NAME}` in an MCP server's environment or headers. The value is resolved only for that agent's runs.

Values are encrypted with `WORLD_SECRETS_KEY` (32 random bytes, base64). The app generates one on first start. To rotate it, set the new key as `WORLD_SECRETS_KEY` and the old one as `WORLD_SECRETS_KEY_PREVIOUS` (comma separated when there are several); the API re-encrypts every saved key on startup.

Provider credential names such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, `OPENROUTER_API_KEY` and `XAI_API_KEY` are reserved, so a task cannot switch an agent's provider account.

## `config.yaml`

Optional. The API and runner read it from `CONFIG_PATH` (the app points it at its data folder). Agents, repos, schedules and kit normally live in the database and are managed in the app.

```yaml
repos:
  - repo: owner/repo
    branch: main

agent:
  backends:
    claude: {}
    codex: {}

mcp_servers:
  - name: linear
    command: npx
    args: ["-y", "@linear/mcp-server"]
    env:
      LINEAR_API_KEY: "${LINEAR_API_KEY}"

signing:
  ssh_key_path: /path/to/key
  allowed_signers_path: /path/to/allowed_signers
```

- `backends` lists the CLIs checked at startup; the first one is the default. Models are discovered from each CLI.
- A value that is exactly `${VAR}` is replaced with that environment variable when it is set.
- Commit signing uses the `signing` block, or `GIT_SIGNING_KEY` and `GIT_ALLOWED_SIGNERS`.

## CLI sign-in

Loka uses the logins you already have:

- `claude`: `claude login`, or `CLAUDE_CODE_OAUTH_TOKEN`
- `codex`: `codex login`
- `cursor`: `cursor-agent login`
- `omp`: provider keys in its `models.yml`
- `opencode`: `OPENROUTER_API_KEY`, and only with `WORLD_ENABLE_OPENCODE=1`

Missing or signed-out CLIs show up in the app with the next step to fix them.

## Settings

Everything has a working default.

| Variable | Default | Effect |
|---|---|---|
| `WORLD_TASK_TIMEOUT_SECS` | 3600 | Default time limit for task, follow-up and review runs |
| `WORLD_BACKGROUND_WAIT_SECS` | 1200 | How long a Claude run stays open for background commands still running when its turn ends; `0` closes it at once |
| `WORLD_TOOL_INDEX` | on | `off` gives every agent the full World tool list instead of a small core plus `find_tools` |
| `LOKA_PRIVATE_TEMP_ROOT` | system temp, or `/tmp` | Folder that holds each quest's private temp folder (`loka-q-<id>`). Set it only when `/tmp` is not writable; keep it under 13 characters, or Claude shells fall back to a shared folder |
| `WORLD_TASK_SANDBOX` | `auto` | `auto` (owner quests stay sandboxed), `strict` (fail if the sandbox cannot start) or `off` |
| `WORLD_CLAUDE_EFFORT` | `medium` for chat, `high` for work | Claude Code effort when a message does not pick one |
| `WORLD_REVIEWS` | on | `off` disables independent reviews |
| `WORLD_MAX_REVIEW_ROUNDS` | 2 | Fix rounds after a review asks for changes |
| `WORLD_COMPACT_TOKENS` | 120000 | Context size after which a main session compacts |
| `WORLD_WARM_SESSIONS`, `WORLD_WARM_IDLE_SECS` | sized from CPU and memory, 1800 | Long-lived main-session processes and their idle timeout |
| `WORLD_INBOX_CONCURRENCY`, `WORLD_RUN_CONCURRENCY` | sized from CPU and memory | Agents answering at once, and tasks running at once |
| `WORLD_STEER_INTERRUPT_SECS`, `WORLD_TOOL_HANG_SECS` | 180, 900 | When a steer interrupts a running tool, and when a hung tool is stopped |
| `WORLD_MERGE_CHECK_SECS` | 1800 | How often open pull requests are checked for merges and feedback |
| `WORLD_CI_FEEDBACK` | off | `on` sends failing CI back to every quest, not only those that ask |
| `WORLD_SILENT_SECS` | 600 | A Cursor run that prints nothing for this long is retried once (0 turns it off) |
| `WORLD_MCP_HUB`, `WORLD_MCP_HUB_IDLE_SECS` | on, 60 | One shared process per MCP server, and how long an unused one keeps running |
| `WORLD_API_COMMITS` | on | Quests publish their commits through the GitHub API so GitHub signs them; `off` uses a signed `git push` only |
| `WORLD_BROWSER`, `WORLD_BROWSER_ENGINE` | on, Chrome | Browser tool for agents |
| `WORLD_BROWSER_LOCAL` | off | `on` lets the agent browser open localhost and private addresses |
| `WORLD_FEEDS_ALLOW_PRIVATE` | off | Lets feeds and skill downloads reach private addresses |
| `WORLD_ENABLE_OPENCODE` | off | Offers the OpenCode CLI |
| `WORLD_PRICING`, `WORLD_PRICING_FETCH` | built in, on | List price overrides for CLIs that report tokens but not dollars, and the twice-daily refresh from OpenRouter's public model list |
| `WORLD_DOCS_URL` | `https://loka.ygpt.dev` | Where the `read_docs` agent tool reads Loka's docs |
| `WORLD_SEARCH_INDEX` | on | `off` skips building search indexes; search still works by scanning |
| `LINEAR_WEBHOOK_SECRET`, `CLICKUP_WEBHOOK_SECRET` | none | Webhooks without a secret are rejected |

## Running the pieces by hand

For development only; see [CONTRIBUTING.md](../CONTRIBUTING.md). The API needs `DATABASE_URL`, `AUTH_DISABLED=true`, `API_BIND_HOST=127.0.0.1` and `LOKA_APP_SESSION_FILE` (where it reads or writes the app session; `loka-api login-link` prints a one-time link that hands a browser the cookie). `AUTH_DISABLED_ORIGINS` adds exact extra origins to the allowlist. The runner needs `WORLD_RUNNER_URL`, `WORLD_RUNNER_ID`, `WORLD_RUNNER_TOKEN` and `WORLD_RUNNER_ROOT`. `AUTH_DISABLED=true` makes every request the one local user, which is safe only while the API listens on `127.0.0.1`.
