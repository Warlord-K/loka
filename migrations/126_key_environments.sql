CREATE TABLE IF NOT EXISTS world_environment_variants (
  owner_user_id UUID NOT NULL,
  name          TEXT NOT NULL,
  environment   TEXT NOT NULL CHECK (environment ~ '^[a-z][a-z0-9-]{0,31}$' AND environment <> 'default'),
  ciphertext    BYTEA NOT NULL,
  nonce         BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (owner_user_id, name, environment),
  FOREIGN KEY (owner_user_id, name)
    REFERENCES world_environment(owner_user_id, name) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS world_environment_choices (
  agent_id      UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  owner_user_id UUID NOT NULL,
  name          TEXT NOT NULL,
  environment   TEXT NOT NULL,
  expires_at    TIMESTAMPTZ,
  chosen_by     TEXT NOT NULL DEFAULT 'person',
  chosen_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (agent_id, name),
  FOREIGN KEY (owner_user_id, name, environment)
    REFERENCES world_environment_variants(owner_user_id, name, environment) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_world_environment_choices_owner ON world_environment_choices (owner_user_id);

CREATE OR REPLACE FUNCTION world_environment_choice_owner() RETURNS TRIGGER AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM agent_profiles a WHERE a.id = NEW.agent_id AND a.owner_user_id = NEW.owner_user_id) THEN
    RAISE EXCEPTION 'key environment choices need an agent of the same owner' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_environment_choice_owner ON world_environment_choices;
CREATE TRIGGER world_environment_choice_owner
BEFORE INSERT OR UPDATE ON world_environment_choices
FOR EACH ROW EXECUTE FUNCTION world_environment_choice_owner();

CREATE TABLE IF NOT EXISTS world_command_rules (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  agent_id      UUID REFERENCES agent_profiles(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('allow', 'deny')),
  rule          TEXT NOT NULL CHECK (char_length(rule) BETWEEN 1 AND 200),
  created_by    TEXT NOT NULL DEFAULT 'person',
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_world_command_rules_unique
  ON world_command_rules (owner_user_id, COALESCE(agent_id, '00000000-0000-0000-0000-000000000000'::uuid), kind, rule);

CREATE OR REPLACE FUNCTION world_command_rule_owner() RETURNS TRIGGER AS $$
BEGIN
  IF NEW.agent_id IS NOT NULL
     AND NOT EXISTS (SELECT 1 FROM agent_profiles a WHERE a.id = NEW.agent_id AND a.owner_user_id = NEW.owner_user_id) THEN
    RAISE EXCEPTION 'command rules need an agent of the same owner' USING ERRCODE = 'check_violation';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_command_rule_owner ON world_command_rules;
CREATE TRIGGER world_command_rule_owner
BEFORE INSERT OR UPDATE ON world_command_rules
FOR EACH ROW EXECUTE FUNCTION world_command_rule_owner();

CREATE OR REPLACE FUNCTION world_environment_drop_agent_choices() RETURNS TRIGGER AS $$
BEGIN
  DELETE FROM world_environment_choices WHERE agent_id = NEW.id;
  DELETE FROM world_command_rules WHERE agent_id = NEW.id;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_environment_agent_choices_changed ON agent_profiles;
CREATE TRIGGER world_environment_agent_choices_changed
AFTER UPDATE OF owner_user_id ON agent_profiles
FOR EACH ROW
WHEN (OLD.owner_user_id IS DISTINCT FROM NEW.owner_user_id)
EXECUTE FUNCTION world_environment_drop_agent_choices();
