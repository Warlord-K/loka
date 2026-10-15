CREATE INDEX IF NOT EXISTS idx_world_events_run_type ON world_events (run_id, type) WHERE run_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_world_events_bell_agent ON world_events (agent_id, seq)
  WHERE agent_id IS NOT NULL
    AND (type = ANY (ARRAY['task.finished', 'task.merged', 'task.pr_closed', 'attention.requested', 'agent.budget', 'org.budget', 'timer.done', 'slack.notification', 'todo.nudge', 'report.created', 'agent.transferred', 'channel.message'])
      OR (type = 'agent.status' AND payload->>'status' = 'error'))
    AND COALESCE(payload->>'quiet', '') <> 'true';

CREATE INDEX IF NOT EXISTS idx_world_events_bell_replies ON world_events (agent_id, seq)
  WHERE agent_id IS NOT NULL
    AND type = 'message.created' AND payload->>'author_kind' = 'agent' AND payload->>'author_agent_id' IS NULL
    AND NOT payload ? 'thread_user_id' AND COALESCE(payload->>'quiet', '') <> 'true';

CREATE INDEX IF NOT EXISTS idx_world_events_bell_unowned ON world_events (org_id, seq)
  WHERE agent_id IS NULL
    AND (type = ANY (ARRAY['task.finished', 'task.merged', 'task.pr_closed', 'attention.requested', 'agent.budget', 'org.budget', 'timer.done', 'slack.notification', 'todo.nudge', 'report.created', 'agent.transferred', 'channel.message'])
      OR (type = 'agent.status' AND payload->>'status' = 'error'))
    AND COALESCE(payload->>'quiet', '') <> 'true';
