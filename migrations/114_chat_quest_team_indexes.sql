CREATE INDEX IF NOT EXISTS idx_agent_messages_thread
  ON agent_messages (agent_id, (COALESCE((meta->>'thread_user_id')::uuid, CASE WHEN author_kind = 'user' THEN author_user_id END)), created_at);

CREATE INDEX IF NOT EXISTS idx_agent_messages_elsewhere
  ON agent_messages (author_agent_id, created_at) WHERE author_kind = 'agent' AND author_agent_id <> agent_id;

CREATE INDEX IF NOT EXISTS idx_agent_sessions_plan_awaiting
  ON agent_sessions (task_id) WHERE plan_state = 'awaiting' AND status <> 'archived';

CREATE INDEX IF NOT EXISTS idx_agent_tasks_org_updated ON agent_tasks (org_id, updated_at DESC NULLS LAST);

CREATE INDEX IF NOT EXISTS idx_session_runs_agent_started
  ON session_runs (agent_id, started_at) INCLUDE (cost_usd) WHERE started_at IS NOT NULL;
