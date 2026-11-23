ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS context_sent JSONB NOT NULL DEFAULT '{}'::jsonb;
