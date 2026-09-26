CREATE TABLE IF NOT EXISTS api_tokens (
  id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  token_hash   BYTEA NOT NULL UNIQUE,
  prefix       TEXT NOT NULL,
  scope        TEXT NOT NULL CHECK (scope IN ('read', 'write')),
  agent_ids    UUID[],
  created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  last_used_at TIMESTAMPTZ,
  revoked_at   TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS api_tokens_user_idx ON api_tokens (user_id, created_at DESC);
