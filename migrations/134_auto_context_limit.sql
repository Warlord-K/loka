ALTER TABLE agent_profiles
  ADD COLUMN IF NOT EXISTS auto_context_tokens INTEGER
    CHECK (auto_context_tokens IS NULL OR auto_context_tokens >= 10000),
  ADD COLUMN IF NOT EXISTS auto_context_action TEXT NOT NULL DEFAULT 'compact'
    CHECK (auto_context_action IN ('compact', 'reset'));

ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS auto_compact_floor INTEGER;
