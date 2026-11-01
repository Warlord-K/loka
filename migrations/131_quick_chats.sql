ALTER TABLE agent_sessions DROP CONSTRAINT IF EXISTS agent_sessions_role_check;
ALTER TABLE agent_sessions ADD CONSTRAINT agent_sessions_role_check
  CHECK (role IN ('main', 'task', 'aside', 'review', 'designer', 'direct', 'quick'));

ALTER TABLE session_runs DROP CONSTRAINT IF EXISTS session_runs_kind_check;
ALTER TABLE session_runs ADD CONSTRAINT session_runs_kind_check
  CHECK (kind IN ('turn', 'task', 'review', 'aside', 'followup', 'publish', 'direct', 'quick'));

CREATE INDEX IF NOT EXISTS idx_agent_sessions_quick
  ON agent_sessions (agent_id, updated_at DESC) WHERE role = 'quick';

CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_sessions_thread_fork
  ON agent_sessions (agent_id, (asked_by->>'thread_root'))
  WHERE role = 'quick' AND status <> 'archived' AND asked_by ? 'thread_root';
