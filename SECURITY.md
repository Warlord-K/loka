# Security

Loka agents run real coding CLIs with your real credentials, read untrusted text (repositories, issues, Slack, RSS feeds) and can open pull requests. We treat any way around the rules below as a vulnerability.

## Reporting a vulnerability

Please report privately through GitHub's **Report a vulnerability** button on the repository's Security tab, not in a public issue. Include what you did, what you expected and what happened, and the version or commit. We aim to acknowledge reports within three working days and to ship a fix or mitigation for serious issues within thirty days, and we will credit you unless you ask us not to.

In scope: the desktop app, the API, the runner and the web app it serves. Prompt injection counts when it gets an agent past one of the boundaries below, not when it only changes what an agent says.

## Local only

Loka runs for one person on one Mac. The app starts Postgres, `loka-api` and the web app bound to `127.0.0.1` only, and Postgres has no Unix socket. The API runs with `AUTH_DISABLED=true`, so every request acts as the single local user. That is safe only because nothing outside this Mac can reach those ports. Never bind the API or the web app to another interface, forward their ports or put them behind a proxy; anyone who can reach them controls your agents and everything they can run.

The runner gets no database credentials: it holds a runner token (stored hashed) and leases one job at a time over the API. Loka allows one runner per person.

## Boundaries we defend

- **Owner verification.** Inbox items are stamped in SQL with whether you sent them; payload values cannot override the stamp. Only a message you wrote unlocks actions on your own setup. Text from repositories, issues, Slack, feeds and webhooks is marked as not from the owner.
- **Agent tools.** Agents get the World tools their powers allow. Agents cannot install anything globally, change global rules, change any agent's approval mode or pause themselves; you do that in the app. Task sessions get read-only World tools.
- **Sandboxing.** Quests and folder sessions run the way each CLI ships, and the agent's approval mode (read only, auto, full) is the limit. With `WORLD_TASK_SANDBOX=strict`, Claude task runs use Claude Code's Bash sandbox with credential paths hidden and writes limited to the worktree, its git common dir, temp and package caches; Codex follows the agent's approval mode; Cursor uses its own sandbox with a per-run policy; omp and OpenCode are wrapped in `sandbox-exec`. CLIs get an allow-listed environment plus only the keys granted to that agent. Runtime git ignores repository hooks, `core.fsmonitor`, repo-level filters, merge drivers and ssh or gpg programs, and repo names and refs are validated before git sees them. Host secrets are redacted from run output.
- **Chat turns.** Main turns and asides never get tools that act on the machine: Claude runs with no tools (or read-only skill files), Codex always runs `read-only` with its shell off when the sandbox cannot hold it, Cursor runs in ask mode, oh-my-pi runs `--no-tools`, and OpenCode gets an agent with every built-in tool off.
- **Agent browser.** The agent browser runs through `loka-agent browser`, the only command Claude's sandbox lets out, pinned to the runner binary's absolute path, the agent's own id and its mode. It opens only `http` and `https`, refuses localhost and private addresses unless you set `WORLD_BROWSER_LOCAL=on`, checks the page address after every command, refuses launch options, uploads, downloads, file arguments and raw CDP, and keeps each agent's profile and the browser's own state out of the Bash sandbox.
- **Secrets.** Saved key values are encrypted with `WORLD_SECRETS_KEY` (AES-256-GCM, bound to the owner and key name), which the app generates and keeps in its data folder. They are never returned by the API and are masked in MCP and model configs. To rotate, set the new value as `WORLD_SECRETS_KEY` and the old one in `WORLD_SECRETS_KEY_PREVIOUS`; the API re-encrypts every stored key on startup.
- **MCP servers and their keys.** Resolved `${secret:KEY}` values in MCP servers never reach a file on disk or a process argument (per-agent omp model configs are the one exception: they are written to a private 0600 per-run directory that is removed after the run). The runner's MCP hub holds them in memory: command servers start from the hub with the values only in their own environment, and CLIs reach them through a `loka-agent mcp-shim` that names the server and nothing else. The hub accepts a shim only when its parent process chain belongs to a running job. Remote servers whose headers carry keys go through a loopback proxy in the hub that adds the headers itself; the CLI only holds a per-agent proxy token that stops working when the run ends. A changed or revoked key stops the servers using it within one lease heartbeat (15 s).
- **Host configuration stays on the host.** Agents get only the MCP servers they are equipped with: Claude always runs with `--strict-mcp-config`, Codex runs with plugins and apps off and every server from `$CODEX_HOME/config.toml` and the project's `.codex/config.toml` disabled, Cursor denies the tools of servers in `~/.cursor/mcp.json` and the project's `.cursor/mcp.json`, OpenCode disables the servers of the global config, and omp gets a `disabledServers` list of the servers it would import from other tools.
- **Web app.** Every page carries a strict Content-Security-Policy with a per-request nonce (`script-src 'nonce-...' 'strict-dynamic'`, no inline handlers or `javascript:` links), a short list of image, frame and connect origins, `frame-ancestors 'none'` and `object-src 'none'`. Uploads that are not images are served with `nosniff` and attachment disposition.
- **Desktop app.** Workbench and terminal commands check the calling origin, refuse paths that escape the shared folder or follow symlinks out of it, and refuse writes into `.git`. Slack capture runs with a cleared environment and its tokens never leave the Mac.
- **Outbound requests.** Fetches of user-supplied URLs (feeds, skills) refuse private, loopback, link-local and metadata addresses after DNS resolution and on every redirect, and ignore proxy settings, unless you set `WORLD_FEEDS_ALLOW_PRIVATE`.

## Known limits

- Any process running on this Mac can reach the API on `127.0.0.1` without signing in. Loka trusts your local user account; do not run it on a Mac shared with people you do not trust.
- With `WORLD_TASK_SANDBOX=off`, task runs have full access to everything your user account can reach. Claude agents whose approval is `auto` also get full access when Claude Code refuses auto mode. Keep the default `auto`.
- Every agent's jobs run as your user, so a prompt-injected task of one agent can read other agents' transcripts and worktrees when its sandbox allows reads there. Keep agents on Claude with the sandbox on, or use separate Macs, when that matters.
- In-page requests from a public site the agent browser opened can still reach local services that answer cross-origin requests; turning on `WORLD_BROWSER_LOCAL` widens that.
- The desktop app trusts the local web origin it loads with terminal and file access on your Mac, so script injection on that origin would reach your Mac. The Content-Security-Policy blocks injected markup scripts, but report anything that renders untrusted HTML.

## Keeping it safe

- Leave the API and web app on `127.0.0.1`. If you run the pieces by hand for development, set `API_BIND_HOST=127.0.0.1`.
- Back up the app's data folder (`~/Library/Application Support/dev.ygpt.loka`), which holds the database and the secrets key, and keep it private.
- Give `gh` and any GitHub token access only to the repositories agents should touch.
- Set `LINEAR_WEBHOOK_SECRET` and `CLICKUP_WEBHOOK_SECRET` if you use those webhooks; webhooks without a secret are rejected.
