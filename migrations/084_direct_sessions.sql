ALTER TABLE agent_sessions DROP CONSTRAINT IF EXISTS agent_sessions_role_check;
ALTER TABLE agent_sessions ADD CONSTRAINT agent_sessions_role_check
  CHECK (role IN ('main', 'task', 'aside', 'review', 'designer', 'direct'));

ALTER TABLE session_runs DROP CONSTRAINT IF EXISTS session_runs_kind_check;
ALTER TABLE session_runs ADD CONSTRAINT session_runs_kind_check
  CHECK (kind IN ('turn', 'task', 'review', 'aside', 'followup', 'publish', 'direct'));
