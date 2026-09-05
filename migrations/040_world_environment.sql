CREATE TABLE IF NOT EXISTS world_environment (
  owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  ciphertext    BYTEA NOT NULL,
  nonce         BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (owner_user_id, name)
);

CREATE TABLE IF NOT EXISTS world_environment_agents (
  owner_user_id UUID NOT NULL,
  name          TEXT NOT NULL,
  agent_id      UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  PRIMARY KEY (owner_user_id, name, agent_id),
  FOREIGN KEY (owner_user_id, name)
    REFERENCES world_environment(owner_user_id, name) ON DELETE CASCADE
);

CREATE OR REPLACE FUNCTION world_environment_revoke_agent_grants()
RETURNS TRIGGER AS $$
BEGIN
  DELETE FROM world_environment_agents WHERE agent_id = NEW.id;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_environment_agent_changed ON agent_profiles;
CREATE TRIGGER world_environment_agent_changed
AFTER UPDATE OF owner_user_id, visibility ON agent_profiles
FOR EACH ROW
WHEN (OLD.owner_user_id IS DISTINCT FROM NEW.owner_user_id OR OLD.visibility IS DISTINCT FROM NEW.visibility)
EXECUTE FUNCTION world_environment_revoke_agent_grants();
