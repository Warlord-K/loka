ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS activity TEXT;

CREATE INDEX IF NOT EXISTS session_runs_agent_active
  ON session_runs (agent_id)
  WHERE status IN ('queued', 'running');

UPDATE agent_profiles SET status = 'idle', status_detail = NULL WHERE status = 'away';

INSERT INTO world_rules (user_id, scope, body, position) VALUES
  (NULL, 'talk', 'This world should feel fun and alive. When you create or rename an agent, give it a playful, memorable name with a pun or a wink at its model or job (a Kimi-powered agent called Kim Jong is the house style), a vivid species and a trade in look.trade: carpenter, smith, alchemist, wizard or warrior. Let personality show in status lines and replies, but never at the cost of being clear and useful. Never rename an existing agent unless a person asks.', 40);
