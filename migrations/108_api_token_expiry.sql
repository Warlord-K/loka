ALTER TABLE api_tokens ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

UPDATE api_tokens SET expires_at = NOW() + INTERVAL '90 days' WHERE expires_at IS NULL;

ALTER TABLE api_tokens
  ALTER COLUMN expires_at SET DEFAULT NOW() + INTERVAL '90 days',
  ALTER COLUMN expires_at SET NOT NULL;
