---
name: suggest-quests
description: Scan the repos and signals you work with, find a few worthwhile quests and put them on your person's To do list as suggestions they approve. Use on a suggestion schedule or when asked to suggest work. Never starts work itself.
---
# Suggest quests

You look for work worth doing and offer it. Your person decides. You never start, assign or create a quest that changes anything from a scan; only an approved suggestion does that.

## 1. Remember what was already said

1. Read your notes. Keep a section `## Suggestions` with one line per suggestion you made: a short key (`repo:area:kind`, like `acme/api:billing:flaky-test`), the date, and its fate (`open`, `approved`, `dismissed`, `done`).
2. Call `list_todos` with `include_done` true. For each to-do you added that carried suggestions: approved ones are `approved`, struck off ones `done`, cancelled ones `dismissed`. Update the lines.
3. Call `list_tasks` for open quests and `list_todos` for open to-dos. Anything already a quest, a to-do or `dismissed` in your notes is off the table. A dismissed idea comes back only when something material changed (it broke production, it got much worse), and then say so in the to-do.

## 2. Gather signals

Use what you can reach; skip what you cannot, without asking.

- **Recent failures**: `world_usage` (7 to 14 days) for failures grouped by cause, the agents and quests that failed, time limit hits and the costliest quests. Repeated causes are candidates: a flaky test, a missing key, a quest kind that always times out. For a cause worth a closer look, `run_history` lists the failed runs and `run_log` shows what a run did, so the to-do can name the failing test or command.
- **Feeds**: recent `feed.item` events and alerts you were woken for (RSS, webhooks, Alertmanager). An alert that fired more than once is a candidate.
- **Repos**: your chat turns cannot read code, so use what task reports and reviews of recent quests (`list_tasks`, `my_sessions`) already told you: TODO and FIXME hotspots, skipped or flaky tests, outdated or vulnerable dependencies, slow builds. Only when your person asked for code scans in the schedule's prompt may you start one read-only quest per scan, titled "Suggestion scan: owner/repo" with `time_limit_minutes` 15, whose brief says to change nothing, run `git grep -n "TODO\|FIXME"`, the package manager's outdated check and the test suite once, and report candidates only. File suggestions from its report.
- **Issues**: open issues or tickets without an owner that you have seen in events, webhooks, Slack or Linear.
- **Your person's words**: things they mentioned wanting "some day" in chat.

## 3. Pick a few

Score each candidate on value (what it unblocks, risk it removes, time it saves) against effort (a short quest, an afternoon, a project). Keep the best 1 to 3 per scan. Fewer is better than noise; zero is a fine result. Prefer small, concrete, verifiable work with a clear done condition over vague clean-ups.

## 4. File each one

Call `add_todo` with:

- `text`: the outcome in a few words ("Fix the flaky upload test in acme/api").
- `suggested`: true.
- `priority`: normal unless it is hurting people now.
- `source_url` and `source_label`: the evidence (issue, failing run, alert, file permalink) when there is a link.
- `actions`: one to three suggestions. The first is usually a quest:
  - `kind` task, `label` like "Start quest: fix flaky upload test", `title`, `agent` (yourself, or the agent whose expertise fits from `find_expert`), `repo` when there is one,
  - `body`: the full brief, what to do, what done looks like, how to check it, then a last line `Review: yes` or `Review: no` and `Time limit: N minutes` with your estimate plus a third.
  - Add a cheaper alternative when there is one (`kind` other, "Just investigate and report"), or a `message` to the person who owns the area.

Then add one line per suggestion to your notes with `open`, and save them with `update_my_notes`.

## 5. Report

End the scan silently when you filed nothing. When you filed something, say in one or two sentences how many suggestions are on the To do list and the top one. Do not repeat the briefs.

## When a suggestion is approved

The approval comes back to you as a message naming the to-do. For a quest pick, use `start_task` with that title and brief, `review` and `time_limit_minutes` as the brief's last lines say (the person's edits win), in the same turn. Mark the line `approved` in your notes. Strike the to-do off with `complete_todo` once the quest is done.

## When a suggestion is dismissed

You get a `todo.dismissed` event. Mark the line `dismissed` in your notes with `update_my_notes` and say nothing.
