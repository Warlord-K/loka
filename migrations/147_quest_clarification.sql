ALTER TABLE agent_sessions DROP CONSTRAINT IF EXISTS agent_sessions_plan_state_check;
ALTER TABLE agent_sessions ADD CONSTRAINT agent_sessions_plan_state_check
  CHECK (plan_state IN ('planning', 'clarifying', 'awaiting', 'approved'));
