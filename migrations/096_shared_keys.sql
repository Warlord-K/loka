CREATE TABLE IF NOT EXISTS world_shared_environment (
  name        TEXT PRIMARY KEY,
  ciphertext  BYTEA NOT NULL,
  nonce       BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
  note        TEXT NOT NULL DEFAULT '' CHECK (char_length(note) <= 500),
  updated_by  UUID REFERENCES users(id) ON DELETE SET NULL,
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS world_shared_environment_agents (
  name     TEXT NOT NULL REFERENCES world_shared_environment(name) ON DELETE CASCADE,
  agent_id UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  PRIMARY KEY (name, agent_id)
);

CREATE INDEX IF NOT EXISTS world_shared_environment_agents_agent
  ON world_shared_environment_agents (agent_id);

CREATE OR REPLACE FUNCTION world_environment_revoke_agent_grants()
RETURNS TRIGGER AS $$
BEGIN
  DELETE FROM world_environment_agents WHERE agent_id = NEW.id;
  DELETE FROM world_shared_environment_agents WHERE agent_id = NEW.id;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_environment_agent_changed ON agent_profiles;
CREATE TRIGGER world_environment_agent_changed
AFTER UPDATE OF owner_user_id ON agent_profiles
FOR EACH ROW
WHEN (OLD.owner_user_id IS DISTINCT FROM NEW.owner_user_id)
EXECUTE FUNCTION world_environment_revoke_agent_grants();
