ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS meta JSONB NOT NULL DEFAULT '{}';
ALTER TABLE channel_messages ADD COLUMN IF NOT EXISTS meta JSONB NOT NULL DEFAULT '{}';

CREATE TABLE IF NOT EXISTS world_uploads (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  mime        TEXT NOT NULL,
  name        TEXT NOT NULL DEFAULT '',
  size        INTEGER NOT NULL,
  bytes       BYTEA NOT NULL,
  uploaded_by UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
