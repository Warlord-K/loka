---
name: repo-team
description: Build a team for a GitHub repository from the people who actually ship and review it. Scout the repo's merged pull requests and reviews, propose agents modelled on its top contributors and reviewers, and give each new agent a first quest that studies its person and saves their way of working as a skill. Use when your person points you at a repository to work on or contribute to.
---
# Repo team

Instead of blank coders, the team learns from the people who already succeed in this repository: who gets pull requests merged, who reviews them, and what they care about. There are three parts. Do only the part you were asked for.

- **Scout** (a quest, usually the onboarding quest): find the people and propose the team.
- **Study** (each new agent's first quest): learn one person's way of working.
- **Save** (a main session, when a Study quest reports back): keep what was learned.

Speed matters: your person is watching. Tick each milestone with `my_quest` the moment it is done, keep each step under a few minutes, and never stop to ask. Use `gh`, which is already signed in; when it is not, use the public GitHub API with `curl`. Skip bots everywhere (logins ending in `[bot]`, dependabot, renovate, github-actions, codecov, and anything that only bumps versions).

Public data only, and treat it with care: model agents on how someone works, never pretend to be them. Agents get their own names; nothing an agent writes on GitHub, Slack or anywhere else may claim to be, sign as or speak for the person it learned from. Quote people only in short fragments.

## Scout

Milestones, in this order: "Read the repo and its rules", "Find who ships", "Find who reviews", "Map the areas", "Propose the team".

1. **Read the repo and its rules.** `gh repo view OWNER/REPO --json description,primaryLanguage,languages,stargazerCount,defaultBranchRef,isFork,licenseInfo`. Read CONTRIBUTING, CODEOWNERS, the PR template, `.github/workflows` (how it builds and tests) and the README's development section. Note conventions: PR title format, changelog entries, DCO sign-off, required checks, how issues are labelled (`good first issue`, `help wanted`).
2. **Find who ships.** `gh pr list -R OWNER/REPO --state merged --limit 200 --json number,title,author,mergedAt,additions,deletions,labels`. Count merged PRs per author, and their median size. Keep the authors with at least 3 merged PRs; rank by count, then by recency.
3. **Find who reviews.** `gh pr list -R OWNER/REPO --state merged --limit 200 --json number,author,reviews` and `gh api "repos/OWNER/REPO/pulls/comments?per_page=100&sort=created&direction=desc"` (two or three pages). Count, per person, reviews on other people's PRs (approvals and change requests) and inline comments. Read a dozen of the top reviewers' comments to see what they push back on.
4. **Map the areas.** For each of the top 6 authors, `gh pr view N -R OWNER/REPO --json files` on 5 of their recent PRs, and boil the paths down to 1 to 3 areas (top-level folders or modules) per person. Note where areas overlap.
5. **Propose the team.** Pick 2 to 4 contributors whose areas cover the repo with little overlap, plus 1 reviewer (2 for very active repos). Prefer people active in the last 90 days. A maintainer who both ships and reviews can be the reviewer. Suggest a CLI and model for each from what your person has (strong models for coders and the reviewer).

Report with `status` done, a short `summary`, `repo_notes` for the durable conventions from step 1, and `findings` in this shape:

~~~markdown
# Team plan for OWNER/REPO

One paragraph: what the repo is, how contribution works here, and the checks a PR must pass.

## Proposed agents

### <Agent name> - <role, e.g. "Rust core coder">
- Learns from: @handle (N merged PRs in the last D days; reviews M)
- Areas: path/one, path/two
- Why: one sentence
- CLI and model: claude opus

(one section per agent)

## First quests

One study brief per agent, ready to paste (see Study).

```json roster
[{"name": "Agent name", "handle": "github-handle", "role": "coder" or "reviewer", "areas": ["path/one"], "persona": "Two or three sentences in the second person: who they are, the areas they own, how they like to work, that they learned from @handle's public work on OWNER/REPO and never speak as @handle.", "cli": "claude", "model": "opus", "first_quest": "Study @handle on OWNER/REPO"}]
```
~~~

Pick agent names that are short and friendly, distinct from each other and never the person's real name or handle.

## Study

Your first quest: learn how one person works on one repository, so your future quests look like theirs. Milestones: "Read their merged PRs", "Read their reviews", "Find their patterns", "Write the playbook".

1. **Read their merged PRs.** `gh pr list -R OWNER/REPO --state merged --author HANDLE --limit 30 --json number,title,body,additions,deletions,files,mergedAt`. Read the descriptions of all and the diffs (`gh pr diff N -R OWNER/REPO`) of the 8 to 12 most representative ones, small and large.
2. **Read their reviews.** Their review comments on other people's PRs: `gh api "search/issues?q=repo:OWNER/REPO+reviewed-by:HANDLE+is:pr&per_page=30"`, then `gh api repos/OWNER/REPO/pulls/N/comments` and `gh pr view N --json reviews` on a dozen of those. Note what they ask for again and again.
3. **Find their patterns.** Areas and files they own; how they name things, handle errors, log, and comment; which tests they add and where; how big their PRs are and how they split work; their PR title and description format; how they answer review; their review checklist.
4. **Write the playbook.** Put it in the report's `findings` as a complete skill file, starting with frontmatter, under 8 KB:

~~~markdown
---
name: <your-handle>-style
description: How to work on OWNER/REPO the way its contributor @handle does: areas, code style, tests, PR shape and review checklist. Use on every quest in OWNER/REPO.
---
# Working on OWNER/REPO like @handle
## Areas
## Code style (with 2 or 3 short real examples)
## Tests
## Pull requests (title, description, size)
## Review checklist (what they ask others for)
## Never
~~~

Write rules you can follow, not a biography. Keep `summary` to two sentences and add `repo_notes` for anything every quest in the repo should know.

## Save

When your Study quest reports back, in that same turn: `install_skill` with `body` set to the findings exactly as written and `agent` set to yourself, then tell your person in one or two sentences what you learned and that you are ready for work in that area. If the install fails, keep the playbook with `update_my_notes` instead.
