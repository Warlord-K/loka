ALTER TABLE terminal_sessions ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'shell';
ALTER TABLE terminal_sessions ADD COLUMN IF NOT EXISTS agent_id UUID REFERENCES agent_profiles(id) ON DELETE CASCADE;

ALTER TABLE terminal_sessions DROP CONSTRAINT IF EXISTS terminal_sessions_kind;
ALTER TABLE terminal_sessions ADD CONSTRAINT terminal_sessions_kind
  CHECK ((kind = 'shell' AND agent_id IS NULL) OR (kind = 'browser' AND agent_id IS NOT NULL));
