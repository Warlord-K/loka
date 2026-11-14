# Agent CLI backends

Loka drives five coding CLIs: Claude Code (`claude`), Codex (`codex`), oh-my-pi (`omp`), Cursor (`cursor-agent`) and OpenCode (`opencode`, opt-in). This page covers what each backend supports, worked out from the code, and what each one costs in memory and latency, measured through the same code paths World uses.

## Feature map

Dispatch lives in [`agent/src/cli.rs`](../agent/src/cli.rs) (`run`, `relay`, `Steer`). The warm pool is in [`agent/src/world/warm.rs`](../agent/src/world/warm.rs), and model discovery in [`agent/src/model_catalog.rs`](../agent/src/model_catalog.rs).

Every cell still marked **No** carries the reason underneath its table.

### Sessions

| | claude | codex | omp | cursor | opencode (opt-in) |
|---|---|---|---|---|---|
| Fork | `--resume <id> --fork-session` | `exec ... fork <id> -`, app-server `thread/fork` | `--fork <id>` | Copies the chat store (`chats/<md5 cwd>/<id>` under `CURSOR_CONFIG_DIR` or `~/.cursor`) under a new id with SQLite `VACUUM INTO`, then `--resume` | `opencode export`, fresh ids plus the task's directory and project, `opencode import`, then `-s` |
| Resume | `--resume <id>`, new sessions get `--session-id <uuid>` | `exec ... resume <id> -`, app-server `thread/resume` | `--resume <id>` | `--resume <id>` | `-s <id>`, prompt on stdin |
| Rewind | `--resume-session-at <checkpoint> --fork-session` | throwaway app-server: `thread/turns/list`, `thread/fork {lastTurnId}`, then exec resume | rpc `get_branch_messages`, `branch`, `get_state` | Fork with `latestRootBlobId` set back to the kept turn's checkpoint (each run records its chat root) | Fork with the export cut after the kept turn's last message (each run records that message id) |
| Steering into a live run | stream-json user lines on stdin (`--input-format stream-json --replay-user-messages`) | app-server `turn/steer` | rpc `steer`, also on warm turns | Relay: kill the process tree, resume with the message | Relay |
| Interrupt | stdin `control_request` interrupt | `turn/interrupt`, plus killing the command's process tree | rpc `abort_and_prompt` | Kill and resume | Kill and resume |
| Warm long-lived process | `claude -p --input-format stream-json` | `codex app-server` | `omp --mode rpc`, kept per main session | `cursor-agent acp` (Agent Client Protocol); the chat is copied back to the print-mode store after every turn so cold runs and forks see it | `opencode acp` |
| `/compact` | Yes | Yes, app-server `thread/compact/start` | Yes, rpc `compact` | **No** | **No** |

Why not:

- **Cursor `/compact`**: the CLI has no compact command in print or ACP mode, and Cursor summarizes long chats on its own server side. Loka never sees a context size for Cursor to decide on either.
- **OpenCode `/compact`**: OpenCode compacts by itself when a session nears the model's context window (`OPENCODE_DISABLE_AUTOCOMPACT` turns that off), so Loka leaves it to OpenCode.
- **OpenCode checkpoints**: OpenCode keeps every session of the machine in one SQLite database, and on job-uid runners its home is wiped with every lease, so a per-session snapshot would need an export and import per live session around every job. `has_transcript` now resumes while the database exists and starts fresh with a handoff note when it is gone.

### Tools, prompts and inputs

| | claude | codex | omp | cursor | opencode (opt-in) |
|---|---|---|---|---|---|
| MCP config | `--mcp-config <temp json>` | `-c mcp_servers.*` overrides | `mcp.json` in the per-run `PI_CODING_AGENT_DIR` | Temp plugin dir (`--plugin-dir --approve-mcps`) plus `.cursor/cli.json` allows; ACP `mcpServers` on warm turns | `OPENCODE_CONFIG=<temp>/opencode.json` |
| World MCP tools | Yes | Yes | Yes | Yes | Yes |
| Images | base64 stream-json blocks | `--image`, app-server `localImage` | `@path` args in print mode, base64 rpc images on warm turns | Listed in the prompt for its read tool; base64 ACP blocks on warm turns | `--file`; base64 ACP blocks on warm turns |
| System prompt | `--system-prompt` (task runs prepend the Loka preamble) | Prepended to the first prompt | File via `--system-prompt` or `--append-system-prompt` | Prepended to the prompt, skipped on resume | Lean turns: the `loka-lean` agent's `prompt` (`{file:...}`); task runs: prepended |
| Lean, tool-less main turns | `--tools "" --strict-mcp-config --setting-sources ""` | Partial: `--sandbox read-only`, built-in feature tools off, shell off unless there are skill files | `--no-tools --no-skills --no-rules --no-extensions --no-lsp` | `--mode ask`: shell and edits off, MCP tools allowed, reads limited to the agent's home and skill files by the sandbox read boundary | A `loka-lean` agent with every built-in tool off and only `<server>_*` MCP tools on, host Claude Code skills and project config off: about 190 input tokens for a one-word turn |
| Skill files readable in lean turns | Yes, a `Read(/<file>)` allowlist | Partial, the read-only sandbox reads anything | **No** | Yes, listed in the sandbox read boundary | **No** |
| Effort | `--effort` | `-c model_reasoning_effort` | `--thinking` | **No** | `--variant` |
| Fast mode | settings `fastMode` | `-c service_tier="fast"` | **No** | **No** | **No** |
| Model discovery | stream-json `initialize` control request | app-server `model/list` | `omp models --json` | `cursor-agent models` | `opencode models` |

Why not:

- **Codex lean turns are partial**: Codex has no switch that removes its shell and file tools entirely; Loka turns off every feature tool it can and keeps the read-only sandbox.
- **omp and OpenCode skill files**: their lean turns turn every built-in tool off, read included, and neither CLI can allow reading one file while denying the rest. Skills still reach them through the prompt.
- **Cursor effort and fast**: Cursor takes these as per-model bracket parameters (`'claude-opus-4-8[effort=high,fast=false]'`) that only some models accept and that differ by model. Pick the variant in the model id; `cursor-agent models` lists them.
- **omp fast**: omp has no fast or priority tier flag.
- **OpenCode fast**: OpenCode has no fast or priority tier flag; `--variant` covers effort.

### Cost, sandbox and access

| | claude | codex | omp | cursor | opencode (opt-in) |
|---|---|---|---|---|---|
| Dollars | From the CLI, cumulative `total_cost_usd`, stored as deltas | List prices (`pricing.rs`) on thread-cumulative token deltas | Per run `usage.cost.total` | List prices (`pricing.rs`) on its token usage (the CLI reports no dollars; warm ACP turns report no tokens, so they add $0) | Per run, summed `step_finish` `cost`; warm turns take the change in ACP `usage_update` cost |
| Token usage | Yes | Yes (cumulative) | Yes (per run) | Yes, per run from the `result` line; **none** on warm ACP turns | Yes, per run from `step_finish`, per turn from ACP |
| `context_tokens` for compaction | Real value | Real, last turn's `tokenUsage.last` | Real, last assistant message | Falls back to input + cached | Real, last step |
| Sandbox | Claude Code Bash sandbox via `--settings` (`WORLD_TASK_SANDBOX`) | Codex bwrap sandbox (read-only, workspace-write or bypass) | `sandbox-exec` (macOS) or `bwrap` (Linux) around the whole CLI, built from Claude's deny lists (`agent/src/sandbox_wrap.rs`) | Cursor's own `--sandbox enabled` with a per-run `.cursor/sandbox.json`: reads limited to the workspace plus toolchains and skill files, writes to the workspace, git dir, caches and a private `TMPDIR`, network only to GitHub | Same wrapper as omp |
| `approval_mode` | auto / read-only allowlist / full | `--approve-for-me` / read-only / bypass | read-only sandbox for `read_only`; auto and full run inside the sandbox | `--mode plan` for `read_only`; auto and full run inside the sandbox | A `loka-read-only` agent with every writing tool off and denied for `read_only`; auto and full run inside the sandbox |

Why not:

- **Cursor usage on warm turns**: Cursor's ACP server ends a prompt with only a stop reason and sends no usage updates. Cold runs keep reporting usage.
- **Cursor context tokens**: the `result` line sums every model call of the run, so it is not the size of the last context window.
- **What the omp and OpenCode wrapper cannot hide**: the CLI itself runs inside it, so its own login (OpenCode's `auth.json`, omp's per-run `models.yml` and provider keys in its environment) stays readable to its tools; other files in `$HOME` outside the deny lists stay readable; and network is not filtered, since neither Seatbelt nor bwrap can filter by host.

### Process shape

| | What runs | Extra per-run work |
|---|---|---|
| claude | One `claude` process (single-binary runtime) | Temp MCP JSON file with no key values |
| codex | npm `node` shim plus the `codex` binary | A rewind spawns an extra app-server |
| omp | One `bun` process, inside `sandbox-exec` or `bwrap` for task runs | Fresh `PI_CODING_AGENT_DIR` per run with copied `models.yml`, `config.yml`, `mcp.json`, `system.md` |
| cursor | `cursor-agent` (node), or `cursor-agent acp` for warm main turns | Temp plugin dir and in-place edits of `.cursor/cli.json` and `.cursor/sandbox.json`, restored afterwards; a private `TMPDIR`; a fork or rewind copies one chat store |
| opencode | `opencode run`, or `opencode acp` for warm main turns, inside the sandbox wrapper for task runs | Temp `opencode.json`, the lean or read-only agent as `OPENCODE_CONFIG_CONTENT`; the prompt goes on stdin; a fork or rewind runs `opencode export` and `opencode import` |

## Shared MCP servers

Measured on the same Mac (Node 26) with `live_hub_memory`: every agent equipped with GitHub, Context7 and Memory (all shared by default) and Sequential Thinking (keeps per-session state, so one per session), each started the way CLIs start them (`npx -y <package>`, an `npm exec` wrapper plus the node server), initialized and listed. "Before" is one copy per session, "after" goes through the hub with one session per agent and server. RSS is the sum over each process tree; a release `mcp-shim` is 7 MB RSS but only 2.3 MB physical footprint, since most of its RSS is the shared binary.

| Agents | Before | After: servers | After: shims | After: total |
|---|---|---|---|---|
| 5 | 40 processes, 2215 MB | 16 processes (8 instances), 1513 MB | 20 processes, 141 MB | 36 processes, 1655 MB |
| 10 | 80 processes, 4558 MB | 26 processes (13 instances), 1837 MB | 40 processes, 212 MB | 66 processes, 2049 MB |

Each shareable server costs about 190 MB per copy here, so the saving is that times (sessions - 1) for every shareable server: with 20 agents on 5 shareable servers, a warm main session each plus some running tasks (about 1.5 sessions per agent), about 150 copies collapse into 5 per device. Servers that keep state are not shared, and remote servers never start a process. Rerun with `BENCH_AGENTS=<n> cargo test -p loka-agent live_hub_memory -- --ignored --nocapture` after `cargo build --release -p loka-agent`.

## Measured cost

Measured on an 11-core M-series Mac with 18 GB, through World's own code paths: `cli::run` for cold, resume, fork, rewind, compact, task, steer and interrupt, `world::warm::Pool::turn` for warm turns, `AppServer::spawn` and `omp::build_command` for idle processes. Models were the cheapest per backend: Claude `haiku`, Codex `gpt-6-luna`, Cursor `composer-2.5`. omp, Cursor and OpenCode were measured again after the parity work (load 2 to 4), omp and OpenCode on `fal/google/gemini-2.5-flash` through fal's OpenRouter router. Every number is the median of 3 runs (2 for steer and interrupt). Claude and Codex were measured on the current code with a 1-minute load average of 5 to 10; Cursor, omp and OpenCode were remeasured at a load of 2 to 4. The first omp run used `fal/kimi-k3`, a reasoning model, so its first latencies measured the model; the numbers below are the rerun on a fast model. OpenCode was installed from npm (`opencode-ai`) for the rerun, with a fal provider in its config.

Rerun with:

```bash
BENCH_RUNS=3 BENCH_CODEX_MODEL=gpt-6-luna BENCH_CURSOR_MODEL=composer-2.5 cargo test -p loka-agent live_backend_bench -- --ignored --nocapture
BENCH_RUNS=3 BENCH_ONLY=omp,cursor,opencode BENCH_OMP_MODEL=fal/google/gemini-2.5-flash BENCH_OPENCODE_MODEL=fal/google/gemini-2.5-flash WORLD_ENABLE_OPENCODE=1 cargo test -p loka-agent live_backend_bench -- --ignored --nocapture
```

For omp, set `OMP_CONFIG_DIR` to a folder holding the `models.yml` to use, `WORLD_OMP_DIR` to a scratch session dir, the provider key (`FAL_KEY`) in the environment, and `BENCH_OMP_MODEL=fal/kimi-k3`. Other knobs: `BENCH_ONLY=claude,codex`, `BENCH_OPS=cold,resume,warm`, `BENCH_OMP_MODEL`, `BENCH_OPENCODE_MODEL`, `BENCH_STEER_RUNS`, `BENCH_IDLE_ONLY=1`. Each `BENCH {...}` line has the load average next to the timing.

### Memory (RSS of the whole process tree, MB)

| | Long-lived process, idle | Warm process after 3 turns, idle | Peak during a one-word turn | Peak during a small task |
|---|---|---|---|---|
| claude | 164 to 258 (`claude -p --input-format stream-json`) | 214 | 241 | 269 |
| codex | 94 to 213 (`codex app-server`: node shim 35 + codex 60 to 120) | 196 (node 48 + codex 128 + `node_repl` 18) | 201 | 332 |
| omp | 448 (`omp --mode rpc`, one bun process) | 445 | 440 | 457 |
| cursor | `cursor-agent acp` | 262 | 284 | 302 |
| opencode | `opencode acp` | 478 | 638 | 683 |

The `node_repl` helper in a warm Codex session comes from this Mac's own `~/.codex/config.toml` MCP servers, which Codex loads for every run. Lean turns stopped starting Codex's built-in feature tools, which took the peak of a one-word turn from 306 to 201 MB. Claude and omp RSS varies a lot between runs because both are JavaScript runtimes whose heap grows lazily. OpenCode is the heaviest shell: a single `opencode run` peaks around 0.65 GB, and its warm ACP process settles near 0.5 GB.

### Latency (ms)

| Operation | claude (`haiku`) | codex (`gpt-6-luna`) | cursor (`composer-2.5`) | omp (`gemini-2.5-flash`) | opencode (`gemini-2.5-flash`) |
|---|---|---|---|---|---|
| Cold exec, lean one-word turn | 1,564 | 4,188 | 8,202 | 2,208 | 2,188 |
| Resume | 1,658 | 4,427 | 10,939 | 2,046 | 2,249 |
| Fork | 1,313 | 5,103 | 9,178 (chat store copy) | 2,358 | 2,877 (export and import) |
| Rewind | 2,064 | 4,898 | about 7,600 (parity test) | 2,847 | about 3,000 (parity test) |
| `/compact` | 13,093 | 5,100 cold, 1,700 on a running app-server | no | 1,000 | no |
| Task with one shell command | 5,220 | 7,940 | 9,993 | 3,579 | 5,942 |
| Warm process, first turn (spawn + turn) | 1,954 | 3,945 | 11,808 | 1,905 | 2,103 |
| Warm process, reused | 749 | 1,397 | 4,299 | 786 | 874 |
| Steer accepted | under 10 | under 10 | under 10 | under 10 | under 10 |
| Done after a steer while a 25 s command runs | 21,101 | 26,020 | 67,674 | 12,156 | 41,353 |
| Done after an interrupt | 1,296 | 2,111 | 18,874 | 11,305 | 14,712 |
| Idle process ready to take a turn | no ready signal | 649 (`initialize`) | ACP `session/new` 7,000 to 9,000 | 860 (`get_state`) | ACP `session/new` about 300 |

What stands out:

- **Claude is the fastest shell on every operation.** A reused warm Claude process answers in 0.75 s, cold turns take 1.6 s, and an interrupt lands in 1.3 s.
- **A parent Claude Code session slowed every Claude turn.** When the runner starts from inside the Claude desktop app, it inherits `CLAUDE_CODE_ENTRYPOINT=claude-desktop`. That variable makes Claude Code emit a `system/post_turn_summary` event before its `result` line, 0.7 to 4 s after the answer, and World waits for `result`. The child environment now drops it with the other host-session variables (`HOST_SESSION_ENV` in `agent/src/cli.rs`): the tail fell to 7 to 33 ms, and a warm turn from 2.6 s to 0.75 s.
- **Warm reuse pays off for every CLI now**: Codex 1.4 s per reused turn against 4.2 s cold, omp 0.8 s against 2.2 s, OpenCode 0.9 s against 2.2 s, Cursor 4.3 s against 8.2 s. Cursor's ACP server takes 7 to 9 s to open or load a session, so the first warm Cursor turn is slower than a cold one; it pays off from the second turn.
- **Codex lean turns are leaner now, but not lean.** A one-word main turn sent 20,456 input tokens because Codex keeps its built-in tools on under `--sandbox read-only`. Lean turns now switch off the feature tools (multi-agent, image generation, browser and computer use, apps, plugins, goals, view image, skill search, hooks and others), and the shell too unless the turn has skill files to read (`codex::lean_overrides`). That brings it to 13,658 tokens. Claude's tool-less turn sends 621. Most of the remaining 10k is this Mac's own `~/.codex` (the skill list, custom agents, a personal `AGENTS.md`) plus Codex's base instructions. Only a separate `CODEX_HOME` would drop them on a Mac, and copying the OAuth file there risks logging the person out of Codex when tokens rotate.
- **Codex rewinds start with a cold prompt cache.**
- **omp and OpenCode lean turns really are tool-less**: omp sends 255 input tokens for a one-word main turn and OpenCode 194 (it sent about 28,000 with its default tools before the `loka-lean` agent). Cursor's ask mode still sends about 13,600, mostly its own system prompt.
- **omp and OpenCode were fast here because the model was**: both ran `gemini-2.5-flash`, a fast non-reasoning model, so compare their shells with each other, not with Claude on `haiku`.
- **Cursor and OpenCode steering is the slowest**: both still steer by relay, which waits for the tool to finish before it kills and resumes, and every Cursor resume is a cold start of 8 to 11 s. Their ACP servers offer `session/cancel` but no way to add a message to a running turn, so a native steer is not possible yet.

## World runtime

Profiled with `loka-api`, `loka-agent runner` and Postgres 16 with `pg_stat_statements` on the same Mac, release builds, before and after the changes listed below.

### Idle

| | loka-api | loka-agent runner |
|---|---|---|
| RSS | 11 to 15 MB | 8 to 11 MB |
| Threads | 12 (one per core plus the main thread) | 12 |
| CPU | 0.01 s per minute | under 0.01 s per minute |
| DB queries | 46.5 per minute, mostly the 15 s lease and runner sweep | |

### Changes and their effect

| Change | Before | After |
|---|---|---|
| World SSE stream reads new events first and only loads the viewer's scope when there are events; queued wake-ups are drained after each round | 38.1 DB queries per world event with 20 viewers, 0.76 s API CPU for 200 events | 8.1 queries per event, 0.14 s CPU, every event still delivered |
| Runner poll claims shell, terminal and job work at the same time, and only turns inbox items into turns when the poll asks for turns | p50 19.2 ms (both lanes), 21.3 ms (work lane only), 8 queries | p50 6.9 ms and 7.9 ms, 7 queries on the work lane |
| `loka-agent mcp`, `mcp runner` and `mcp-launch` (one per CLI process) use a single-threaded runtime, or none for `mcp-launch` | 13 threads per process | 2 threads |
| The runner's MCP relay finds the job by walking parent pids (`proc_pidinfo` on macOS, `/proc/<pid>/stat` on Linux) instead of running `ps` over the whole process table | one `ps` spawn per MCP message, 19 to 29 ms with 750 to 880 processes | about 4 µs |
| A quiet `!` shell command uploads empty output only every 2 s, to check for a stop | an upload every 400 ms, bounded by the 0.35 s round trip | one every 2 s; stopping can take up to 2 s longer |
| Publish looks for an open PR with one `git rev-list HEAD ^origin/<base>` | 2 `git merge-base` spawns per open PR (up to 400) | 1 spawn |
| The merge watch reads CI checks from its first `gh pr view` | 2 `gh pr view` calls per PR with CI feedback on | 1 |
| Codex lean turns (main turns, asides) switch off the built-in feature tools, and the shell when there are no skill files | 20,456 input tokens and a 306 MB peak for a one-word turn | 13,658 tokens and 201 MB |
| The runner heartbeat keeps the stored capabilities value when it has not changed | a new JSONB value (TOAST write) every 15 s per runner | no rewrite when unchanged |

### Not changed yet

These came up in the audit and are worth doing, but each changes behaviour or needs more care than a safe cleanup:

- Leases are renewed twice: by the per-job heartbeat every 15 s and again by the poll heartbeat, and every wake also nudges each steerable job to heartbeat. A lease heartbeat costs 5 to 7 queries.
- The events endpoint inserts runner events one row at a time and repeats the lease lookup.
- Each SSE viewer still runs `events_since` per event; one shared reader that broadcasts each batch would make it one query per event.
- `JobUid::take` runs `pkill` and wipes the slot dir synchronously inside the runner loop.
- The merge watch checks PRs one after another inside the 60 s sweep.
- All runners share one wake broadcast channel, so every runner's stream wakes for every other runner's wake.
- The terminal PTY output channel is unbounded.
- Per-repo setup (`ensure_repo_identity`, `ensure_commit_hygiene`) runs about 6 git spawns per job. It stays per job on purpose, because an agent can write the shared git dir and the hook has to be re-asserted.
