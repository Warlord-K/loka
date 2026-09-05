ALTER TABLE users
  ADD COLUMN IF NOT EXISTS world_role TEXT CHECK (world_role IN ('view', 'read', 'write', 'admin'));

UPDATE users SET world_role = 'admin' WHERE id = '00000000-0000-0000-0000-000000000000' AND world_role IS NULL;

CREATE TABLE IF NOT EXISTS world_invites (
  token        TEXT PRIMARY KEY,
  email        TEXT NOT NULL,
  role         TEXT NOT NULL CHECK (role IN ('view', 'read', 'write', 'admin')),
  invited_by   UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  accepted_by  UUID REFERENCES users(id) ON DELETE SET NULL,
  accepted_at  TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_world_invites_email ON world_invites (lower(email)) WHERE accepted_at IS NULL;
