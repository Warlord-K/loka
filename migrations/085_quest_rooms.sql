CREATE INDEX IF NOT EXISTS idx_world_events_task ON world_events (task_id, seq DESC) WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_world_events_session ON world_events (session_id, seq DESC) WHERE session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_session_runs_task ON session_runs (task_id, created_at) WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agent_messages_task ON agent_messages ((meta->>'task_id')) WHERE meta ? 'task_id';
