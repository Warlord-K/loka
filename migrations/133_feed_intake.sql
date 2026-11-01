ALTER TABLE agent_feeds
  ADD COLUMN IF NOT EXISTS auth_mode       TEXT NOT NULL DEFAULT 'secret',
  ADD COLUMN IF NOT EXISTS intake          JSONB NOT NULL DEFAULT '{}'::jsonb,
  ADD COLUMN IF NOT EXISTS last_fields     TEXT[] NOT NULL DEFAULT '{}',
  ADD COLUMN IF NOT EXISTS turned_away     BIGINT NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS turned_away_at  TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS turned_away_why TEXT;

ALTER TABLE agent_feeds DROP CONSTRAINT IF EXISTS agent_feeds_auth_mode;
ALTER TABLE agent_feeds ADD CONSTRAINT agent_feeds_auth_mode CHECK (
  auth_mode = 'secret' OR (auth_mode = 'address' AND kind = 'webhook' AND cardinality(allowed_ips) > 0)
);
ALTER TABLE agent_feeds DROP CONSTRAINT IF EXISTS agent_feeds_intake_shape;
ALTER TABLE agent_feeds ADD CONSTRAINT agent_feeds_intake_shape CHECK (
  jsonb_typeof(intake) = 'object' AND octet_length(intake::text) <= 16384 AND cardinality(last_fields) <= 100
);
