ALTER TABLE agent_feeds
  ADD COLUMN IF NOT EXISTS secret_ciphertext BYTEA,
  ADD COLUMN IF NOT EXISTS secret_nonce      BYTEA,
  ADD COLUMN IF NOT EXISTS secret_set_at     TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS secret_shown_at   TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS allowed_ips       TEXT[] NOT NULL DEFAULT '{}',
  ADD COLUMN IF NOT EXISTS auth_grace_until  TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS authed_at         TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS unauth_at         TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS grace_noted_at    TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS auth_failures     INTEGER NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS auth_failed_at    TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS auth_noted_at     TIMESTAMPTZ;

ALTER TABLE agent_feeds DROP CONSTRAINT IF EXISTS agent_feeds_secret_pair;
ALTER TABLE agent_feeds ADD CONSTRAINT agent_feeds_secret_pair CHECK ((secret_ciphertext IS NULL) = (secret_nonce IS NULL));
ALTER TABLE agent_feeds DROP CONSTRAINT IF EXISTS agent_feeds_ip_ranges;
ALTER TABLE agent_feeds ADD CONSTRAINT agent_feeds_ip_ranges CHECK (cardinality(allowed_ips) <= 32);

UPDATE agent_feeds SET auth_grace_until = NOW() + INTERVAL '7 days'
 WHERE kind = 'webhook' AND auth_grace_until IS NULL AND secret_ciphertext IS NULL;
