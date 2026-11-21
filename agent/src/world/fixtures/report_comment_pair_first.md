## Dark mode contrast audit — acme/app frontend

**Scope rendered:** 43 routes, dark mode forced (`colorScheme: 'dark'` + `localStorage['world-theme']='dark'` + `data-theme="dark"`), real backend data (local `loka-api` built from source, sandbox Postgres/Redis, demo-seeded rows). axe-core `color-contrast` + custom DOM-walk (near-white-bg, color-equals-bg) on every route. Full results: `/tmp/darkmode/results.json` (620KB). Screenshots: `/tmp/darkmode-shots/` (43 PNGs, never opened via Read).

### Root cause (one bug, ~120 flagged elements, 13 routes)

The Tailwind `gray` scale is hardcoded as static hex in `frontend/tailwind.config.ts:87-98` (not a `--w-*` CSS-variable token, so it never participates in the light/dark switch — it's just always these values):

```
gray-1 #131210  gray-2 #1a1917  gray-7 #4d4b45  gray-8 #63615a
```

`gray-7`/`gray-8` are used app-wide as "muted/secondary" text (labels, metadata, placeholders, uppercase section headers) against the app's dark panel backgrounds (`gray-1`, `gray-2`, and `--w-paper` dark at `#0b0a09`). The pairing never clears WCAG AA.

### Worst-first findings (dark-mode-scoped routes only)

| Ratio | fg / bg | Needed | Count | Worst example |
|---|---|---|---|---|
| **1.86:1** | `#ffffff` on `bg-sun-9` `#e5b847` | 3:1 (large text) | 1 | `components/agent/[id]` "Edit"/"Save" buttons — `app/(dashboard)/agents/[id]/page.tsx:230,300` |
| 2.01:1 | `gray-7 #4d4b45` / `gray-2 #1a1917` | 4.5:1 | 14 | Sidebar metadata, dashboard-shell |
| 2.09:1 | `gray-7` / `#161513` | 4.5:1 | 5 | Settings panel sub-labels |
| 2.14:1 | `gray-7` / `gray-1 #131210` | 4.5:1 | 15 | Table headers — `app/(dashboard)/settings/page.tsx:144` |
| 2.26:1 | `gray-7` / `#0b0a09` (paper) | 4.5:1 | 18 | Task ticket IDs ("KAR-5") — `components/agent/task-card.tsx:75` |
| 2.83:1 | `gray-8 #63615a` / `gray-2` | 4.5:1 | 38 | Command palette search/section labels — `components/agent/command-palette.tsx:141,147`, `dashboard-shell.tsx:214` |
| 3.19:1 | `gray-8` / `#0b0a09` | 4.5:1 | 27 | Repo/model tags on task cards — `task-card.tsx:92` |

**Per-route breakdown** (node count = number of flagged elements): `/board` 19, `/settings` 17, `/agents/[id]` 13 (includes the white-on-gold button), `/schedules` 13, `/tasks/[id]` 11, `/agents` 10, `/tasks/new` 9, `/chat` 8, `/policies` 5, `/repos` 5, `/chat/[id]` 4, `/repos/[id]` 3, `/schedules/[id]` 3. `/world` and `/admin` are clean (0 issues) once backed by live data. Custom DOM-walk heuristic (near-white-bg, color-equals-bg) found **nothing** on any dark route — this is purely a contrast-ratio problem, not a broken-token/wrong-color-value problem.

**Affected files** (all usages of `text-gray-7`/`text-gray-8` as foreground text, 24 files total — grep used: `rg -l "text-gray-7|text-gray-8" app/ components/`): `app/(dashboard)/{policies,schedules,agents,chat,repos,settings,tasks/new,tasks/[id],agents/[id],schedules/[id],chat/[id],repos/[id]}/page.tsx`, `components/agent/{task-attachments,task-thread-view,review-log-modal,add-repo-dialog,dashboard-shell,agent-column,task-card,command-palette,repo-card,schedule-card,create-schedule-dialog,markdown-editor}.tsx`, `components/ui/page-header.tsx`.

**Suggested fix:** bump the muted-text usages from `gray-7`/`gray-8` to `gray-9` (`#82807a`, ≈4.6:1 on `gray-1`/`#0b0a09`, still fails on `gray-2`) or `gray-10` (`#a09e97`, clears 4.5:1 against every dark panel shade in use) — this is the Tailwind `gray` scale itself, not a `--w-*` token, so it's a legitimate site-wide edit outside the `w-*` token files. For the white-on-gold buttons, swap `text-white` → `text-gray-1`/`#131210` on `bg-sun-9`, consistent with how `sun-10`/`sun-9/15` badges elsewhere already use dark-on-gold.

### Data-landing (light-only) pages — footnote only, not primary findings

`/`, `/login`, `/start`→`/world` redirect path, `/upgrade`, `/reset`, all 28 `/docs/*` routes carry their own (much larger: ~900+ instances) contrast problems, but these are explicitly light-themed via `data-landing`, a deliberate design choice per the repo's own CLAUDE.md, and out of scope for a "dark mode audit." Flagging only because the volume is large enough that it may be worth a separate light-mode pass later: dominant pattern is `#807e7d`/`#a09e9d`/`#8b8988` muted text on `#ffffff`/`#f7f6f3` surfaces, same "secondary gray too light/dark for its background" shape as the dark-mode bug, suggesting the gray scale was never contrast-audited in either direction.

### Screenshots worth viewing
Shown above: `board.png`, `settings.png`, `agent-detail.png`, `schedules.png` (worst 4 by flag count). Full set at `/tmp/darkmode-shots/` if more are wanted; none were opened/read as image bytes at any point — all analysis above came from axe-core/DOM-walk JSON output only.

### Bug found in Loka itself (escalating, not fixing)
While standing up a local API to get real (non-empty) authenticated-app renders, I hit a real bug in `migrations/run.sh`: it runs each `.sql` file via plain `psql -f` with no transaction wrapper, and migration `113_agent_usage_totals.sql` contains a bare `LOCK TABLE` statement that fails outside an explicit transaction block. This breaks fresh-clone bring-up via the documented `docker compose up -d postgres redis migrate` path. I did **not** edit the applied migration (per house rule) — I worked around it locally only (building `loka-api` from source and letting its own `sqlx::migrate!` adopt the already-migrated schema). **This needs to reach the lead so a fix quest gets filed for the right agent** — I'm flagging it here per the explicit team rule rather than patching it myself.

---

<!-- report -->
status: done
summary: Audited all 43 real routes of acme/app's frontend in forced dark mode using Playwright + axe-core color-contrast + a custom DOM-walk. Found one systemic root cause: the hardcoded `gray-7`/`gray-8` Tailwind scale (tailwind.config.ts:87-98, not a `--w-*` token) used as muted text throughout the authenticated dashboard app fails WCAG AA against the app's dark panel backgrounds (ratios 2.01-3.19:1 vs 4.5:1 needed), across ~120 elements on 13 routes, plus one isolated white-on-gold button contrast failure (1.86:1) on the agent detail page. /world and /admin are clean. Data-landing (light-only) pages have a much larger but explicitly out-of-scope contrast problem, noted as a footnote only.
decisions: Built loka-api from source locally (cargo build -p loka-api) against a sandbox Postgres/Redis/config.yaml rather than leaving dashboard routes blank, since AUTH_DISABLED=true alone doesn't seed real data for the (dashboard) route group to render meaningfully. Scoped primary findings to dark-mode-rendered routes only, per brief; data-landing routes get a footnote, not full findings, since their light theme is intentional per repo CLAUDE.md. Did not touch migrations/run.sh or the applied 113 migration despite finding a real bug in it — workaround only, per house rule never to edit a shipped migration.
followups: Escalate to the lead — migrations/run.sh runs migration files via unwrapped `psql -f`, and 113_agent_usage_totals.sql's bare LOCK TABLE statement fails outside an explicit transaction, breaking fresh-clone docker-compose bring-up on the documented path. Needs a fix quest filed for the right agent (likely: wrap migrations/run.sh execution in BEGIN/COMMIT, or add explicit transaction to that migration file as a new numbered migration). Separately: the (dashboard) route group cannot be meaningfully audited against a fresh clone without a running loka-api — worth noting for future QA tasks so they don't hit the same blank-page dead end I did on earlier attempts.
checks: next dev (port 3100, AUTH_DISABLED=true) served all 43 routes 200 OK except /start and /world under strict networkidle wait (both resolved cleanly under domcontentloaded + explicit body-text wait — real-time polling/websocket traffic keeps networkidle from firing, not a site bug). axe-core color-contrast + custom DOM-walk executed on every route. No code changes made; no PR opened; results written only to /tmp/darkmode/results.json and /tmp/darkmode-shots/, both outside the repo.
repo_notes: Dark mode toggles via `data-theme` attribute (not `.dark` class); `--w-*` tokens switch via `:root[data-theme="dark"]:not(:has([data-landing]))`, deliberately structured to win CSS specificity over the light `:root, :root:has([data-landing])` list — confirmed directly in the repo's own CLAUDE.md. `data-landing` subtrees (landing, /docs/*, auth pages) are permanently light by design. Chromium headless-shell executable for local Playwright runs: `~/Library/Caches/ms-playwright/chromium_headless_shell-1248/chrome-headless-shell-mac-arm64/chrome-headless-shell`. Real route list (46 entries) lives in /tmp/pw-test/scan.mjs. Local loka-api bring-up: `docker compose up -d postgres redis`, run migrations/run.sh once (hits the 113 bug, needs manual workaround), then `cargo build -p loka-api --bin loka-api` and run with DATABASE_URL/REDIS_URL/CONFIG_PATH/AUTH_DISABLED=true — on subsequent starts loka-api's own sqlx::migrate! "adopts" migrations/run.sh-applied migrations from schema_migrations rather than re-running them.
<!-- /report -->
