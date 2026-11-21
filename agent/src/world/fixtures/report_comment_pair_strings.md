| route | selector | fg/bg hex | ratio | file:line | suggested fix |
|---|---|---|---|---|---|
| /agents/[id] | button.text-white.bg-sun-9 | #ffffff/#e5b847 | 1.86 | agents/[id]/page.tsx:230,300 | text-gray-1 instead of text-white |
| /board,/settings,/agents,+others | .text-gray-7 (labels) | #4d4b45/#1a1917 | 2.01 | dashboard-shell.tsx:272 | gray-7→gray-10 |
| /settings | th.text-gray-7 | #4d4b45/#161513 | 2.09 | settings/page.tsx:144 | gray-7→gray-10 |
| /settings | p.text-gray-7 | #4d4b45/#131210 | 2.14 | settings/page.tsx:110 | gray-7→gray-10 |
| /board,/tasks/new,/agents/[id] | span.text-gray-7 (ticket id) | #4d4b45/#0b0a09 | 2.26 | task-card.tsx:75 | gray-7→gray-10 |
| /board,/chat,/policies | .text-gray-8 (headers/icons) | #63615a/#1a1917 | 2.83 | command-palette.tsx:141,147 | gray-8→gray-10 |
| /board,/repos,/schedules | span.text-gray-8 (tags) | #63615a/#0b0a09 | 3.19 | task-card.tsx:92 | gray-8→gray-10 |

No workspace changes made (read-only audit, same as before). Nothing else is left to do on this task.

<!-- report -->
status: done
summary: Provided the requested compact worst-first top findings table (7 distinct contrast-failure patterns covering the ~120 flagged dark-mode elements; fewer than 10 rows because duplicates collapse to the same fg/bg/file signature). No new scans or file edits performed.
decisions: Collapsed per-instance duplicates (same fg/bg/file:line repeated across many routes) into one row per unique pattern rather than padding to 10 rows with repeats, since that's a more useful "top issues" list than literal top-10-instances.
followups: Same as prior report — escalate the migrations/run.sh LOCK TABLE bug (113_agent_usage_totals.sql) to the lead for a fix quest.
checks: None run this turn; answer derived entirely from already-computed results.json / prior rg output, no re-scan or re-read of screenshots.
repo_notes: No new facts this turn; see prior report block for full repo_notes.
<!-- /report -->
