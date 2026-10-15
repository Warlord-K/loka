CREATE INDEX IF NOT EXISTS idx_session_runs_created ON session_runs (created_at);

CREATE INDEX IF NOT EXISTS idx_session_runs_agent_spend
  ON session_runs (agent_id, (COALESCE(finished_at, created_at))) INCLUDE (cost_usd);

CREATE INDEX IF NOT EXISTS idx_session_runs_agent_usage
  ON session_runs (agent_id) INCLUDE (cost_usd, input_tokens, cached_input_tokens, output_tokens);

CREATE INDEX IF NOT EXISTS idx_agent_inbox_processing ON agent_inbox (agent_id) WHERE status = 'processing';

CREATE INDEX IF NOT EXISTS idx_runner_run_inbox_inbox ON runner_run_inbox (inbox_id);

CREATE INDEX IF NOT EXISTS idx_agent_messages_author_agent
  ON agent_messages (author_agent_id, created_at) WHERE author_kind = 'agent';

CREATE INDEX IF NOT EXISTS idx_channel_messages_channel_time ON channel_messages (channel_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_parent
  ON agent_sessions (parent_session_id) WHERE parent_session_id IS NOT NULL;
