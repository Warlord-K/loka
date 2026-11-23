ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS request jsonb;

UPDATE session_runs
   SET request = jsonb_strip_nulls(jsonb_build_object(
         'from', substring(prompt from '^\[follow-up from ([^\]\n]+)\]'),
         'text', NULLIF(btrim(substring(prompt from '^\[follow-up from [^\]\n]+?\]\n(.*?)\n\n(?:Answer briefly|\[plan first is off\]|This is feedback on your plan)')), '')))
 WHERE request IS NULL AND kind = 'followup' AND prompt LIKE '[follow-up from %';

UPDATE session_runs
   SET request = jsonb_strip_nulls(jsonb_build_object(
         'from', substring(prompt from '\[aside\] (.+?) pulled you aside'),
         'text', NULLIF(btrim(substring(prompt from 'pulled you aside and asks:\n(.*?)\n\nThis is a (?:quick )?side conversation')), '')))
 WHERE request IS NULL AND kind = 'aside' AND prompt LIKE '%[aside] % pulled you aside%';

UPDATE session_runs
   SET request = jsonb_strip_nulls(jsonb_build_object('focus', NULLIF(btrim(substring(prompt from '\nFocus especially on: ([^\n]*)\n')), '')))
 WHERE request IS NULL AND kind = 'review';

UPDATE session_runs
   SET request = jsonb_strip_nulls(jsonb_build_object(
         'from', 'Reviewer',
         'findings', NULLIF(btrim(substring(prompt from 'Address every finding:\n(.*?)\n\n?(?:Before changing anything|Do the work end to end)')), '')))
 WHERE request IS NULL AND kind = 'task' AND prompt LIKE '%The independent reviewer asked for changes. Address every finding:%';

UPDATE session_runs r
   SET request = jsonb_strip_nulls(jsonb_build_object('text', NULLIF(btrim(t.description), '')))
  FROM agent_tasks t
 WHERE r.request IS NULL AND r.kind = 'task' AND t.id = r.task_id;

UPDATE session_runs
   SET request = COALESCE(request, '{}'::jsonb)
                 || jsonb_build_object('retry', (length(prompt) - length(replace(prompt, E'\n\n[retry]\n', ''))) / length(E'\n\n[retry]\n'))
 WHERE prompt LIKE '%' || E'\n\n[retry]\n' || '%';
