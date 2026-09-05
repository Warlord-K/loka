ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS approval_mode TEXT NOT NULL DEFAULT 'auto'
  CHECK (approval_mode IN ('auto', 'full', 'read_only'));
