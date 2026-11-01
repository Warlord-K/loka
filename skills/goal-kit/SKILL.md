---
name: goal-kit
description: Turn how your person works (lots of email, reading, on call, planning) or a goal in plain words (start a business, run a newsletter) into a proposed setup of agents, connections, routines, skills and the few steps only they can do, for them to approve. Use when your person describes their work or a goal that is not just one GitHub repository.
---
# Goal kit

Your person told you how they work or what they want done. Propose a setup that fits: the agents, connections (MCP servers), routines (watches, feeds, schedules) and skills that would take work off their plate, plus the few steps only they can do. Prefer what already exists and is well regarded over building from nothing. This is a scouting quest: install nothing and change nothing.

Your person is watching, so be quick: tick each milestone with `my_quest` the moment it is done and never stop to ask. Milestones: "Understand how they work", "Match setups to their work", "Find skills and tools", "Check what is trustworthy", "Propose the setup".

1. **Understand how they work.** Restate it in one sentence and split it into 3 to 6 jobs (for a founder: inbox triage, investor updates, hiring pipeline, weekly metrics; for a bakery: website, online orders, bookkeeping, social posts). The brief lists what your person already has installed; do not suggest it again.
2. **Match setups to their work.** Start from the recipes below for every kind of work they mentioned, then fill the gaps.
3. **Find skills and tools.** For jobs the recipes do not cover, search the web and GitHub. Skills (SKILL.md files): the official `anthropics/skills` repository, other vendors' official skill repositories, and curated lists with real use (`gh search repos "<job> SKILL.md" --sort stars`, `gh search code "filename:SKILL.md <job>"`). MCP servers: the official registry (registry.modelcontextprotocol.io), the `modelcontextprotocol/servers` repository and the service's own docs. Prefer the vendor's own skill or server, then well-starred, recently updated ones; prefer remote (URL) servers that need one key over local ones with many dependencies.
4. **Check what is trustworthy.** Open every candidate's SKILL.md or README and read it. Drop anything that runs unexplained scripts, downloads binaries, asks for broad credentials, sends data somewhere unexpected, or has not been touched in a year. Keep the setup small: at most 6 skills, 4 servers, 5 routines and 3 agents, the ones that matter most.
5. **Propose the setup.** Write the steps only your person can do in plain words, in order, each with where to click.

## Recipes

- **Email, calendar and documents.** The Google Workspace server from the `connectors` how-to (call `features connectors` for the exact definition), read-only at first. Steps for them: create a Desktop OAuth client in Google Cloud with the Gmail, Drive and Calendar APIs on, save its id and secret under You > Keys, and sign in with Google on first use. Routines: a weekday morning schedule that triages the inbox into a short digest and drafts replies for approval; a calendar prep schedule before the day starts. Outlook or Microsoft 365 users: look for Microsoft's own server instead.
- **Reading articles, papers or newsletters.** RSS feeds for the sources they named or the well-known ones in their field (`add_feed`, with a filter for their topics), and a weekly schedule that writes a digest report with links. A skill for summarising papers if a good one exists.
- **On call, alerts and incidents.** A watch on their alerts or incidents Slack channel (`subscribe` with the channel), their GitHub notifications for failing checks, and a reviewer-style agent that triages each alert and writes an incident report. When `add_feed` offers the webhook kind, propose an authenticated webhook for their alerting tool (PagerDuty, Grafana, Alertmanager, Sentry); otherwise leave webhooks out.
- **Planning and tickets.** The official Linear or Notion server from the `connectors` how-to, and a weekly schedule that summarises what moved and what is stuck.
- **A GitHub organization.** Their GitHub notifications (You > GitHub) so reviews and mentions reach the right agent, and the repo-team playbook for the one or two repositories they work in most.
- **Starting a business or a side project.** Skills for the jobs (website, bookkeeping, marketing copy), servers for the services they already use, and one or two agents with clear roles.

Report with `status` done, a short `summary` and `findings` in this shape:

~~~markdown
# Proposed setup: <in a few words>

How they work and the jobs, one line each.

## Agents
- **Name** - role, one-line persona, suggested CLI and model.

## Connections (MCP servers)
- **name** - what it does. Source: <URL>. Needs: KEY_NAME (where to get it).

## Routines
- **name** - a watch, feed or schedule, what it does and when.

## Skills
- **name** - what it does for which job. Source: <GitHub URL of the SKILL.md or its folder>. Why trust it: official / N stars, updated <month>.

## Steps for you
1. What to do, where to click, and why.

```json kit
{"agents": [{"name": "...", "persona": "...", "cli": "claude", "model": "sonnet"}],
 "servers": [{"name": "...", "definition": {"type": "http", "url": "https://...", "headers": {"Authorization": "Bearer ${secret:KEY_NAME}"}}, "keys": ["KEY_NAME"], "why": "..."}],
 "routines": [{"kind": "schedule", "agent": "...", "name": "...", "cron": "0 8 * * 1-5", "prompt": "...", "why": "..."}, {"kind": "feed", "agent": "...", "name": "...", "url": "https://...", "filter": "...", "why": "..."}, {"kind": "watch", "agent": "...", "slack_channel": "#alerts", "why": "..."}],
 "skills": [{"name": "...", "url": "https://github.com/.../SKILL.md", "why": "..."}],
 "steps_for_you": ["..."]}
```
~~~

Use `${secret:NAME}` for every key in a server definition, never a value, and name keys the way the vendor's docs do. Leave out any section with nothing in it.
