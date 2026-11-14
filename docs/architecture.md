# Architecture

Loka runs persistent agents (the **World**) for one person on one Mac. The desktop app starts Postgres, `loka-api`, the web app and one `loka-agent runner`, all bound to `127.0.0.1`. [CLAUDE.md](../CLAUDE.md) is the authoritative map of every subsystem and its invariants.

## World runtime

- **Agents** (`agent_profiles`) have an owner, a visibility, a persona, a CLI and model, and powers. Each has one long-lived **main session**, a real CLI session, that holds the conversation with its owner.
- **Inbox.** Messages, quest assignments, Slack captures, feed items and events become `agent_inbox` rows. A trigger stamps each with whether the owner sent it. The runner claims an agent's pending items and runs one **main turn** for the batch. Main turns are lean: no shell and no file access beyond the agent's skills, only the `world` MCP tools. The turn ends with an actions block (`reply`, `start_task`, `ask`, ...).
- **Quests.** `start_task` forks the main session into a **task session** that runs in its own git worktree with full tools inside the sandbox. It ends with a report; an optional reviewer session returns a verdict and the task session fixes findings. The runtime then commits, pushes and opens the pull request, and reports back into the main session. Plan-first, best-of-N attempts, CI feedback, pace and time limits are per-quest options.
- **Untrusted input.** Text that does not come from the owner (repositories, issues, feeds, Slack, webhooks) is marked as such in every prompt, and only the owner's own messages unlock actions on the owner's setup.
- **The runner.** `loka-agent runner` on the Mac polls `/api/runner/{id}/poll` (woken by a long-lived stream), leases jobs and streams events back; it never gets database credentials. It keeps chat turns and work in separate lanes, so a long task never blocks a reply. Each person has one runner.
- **Wakeups.** Nothing polls: inserts fire `pg_notify` on `agent_inbox`, `session_runs`, `schedules` and `world_events`, and workers `LISTEN`. A 60-second sweep only reclaims stale claims and runs the merge watch.
- **Cost.** Every CLI invocation is a `session_runs` row with tokens and dollars; cumulative CLI counters are turned into deltas with per-session baselines. Daily budgets pause an agent when reached.

## Components

| Component | Path | Responsibility |
|---|---|---|
| Desktop app (Tauri) | `desktop/` | Starts and supervises Postgres, the API, the web app and the runner; workbench, terminal, folder sessions, Slack capture |
| API (Rust, Axum) | `api/` | REST, server-sent events, the runner API, external MCP endpoint, Linear and ClickUp webhooks; applies `migrations/` at startup |
| Runner (Rust) | `agent/` | Leases jobs, drives the CLIs, worktrees, publishing; stdio MCP server for World tools; MCP hub |
| Shared crate (Rust) | `shared/` | All SQL and domain logic (`WorldStore` in `shared/src/world.rs`) |
| Web app (Next.js) | `frontend/` | The pixel world, chat, quests, reports and settings |
| Postgres 17 | `migrations/` | Source of truth; triggers drive every wakeup |
