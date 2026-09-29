CREATE OR REPLACE FUNCTION world_handle_key(handle TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE AS $$ SELECT regexp_replace(lower(handle), '[^a-z0-9]', '', 'g') $$;

CREATE INDEX IF NOT EXISTS agent_profiles_handle_key_idx ON agent_profiles (world_handle_key(slug));
CREATE INDEX IF NOT EXISTS agent_slug_aliases_handle_key_idx ON agent_slug_aliases (world_handle_key(slug));

CREATE OR REPLACE FUNCTION world_agent_handle_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('world_agent_handles'));
  IF EXISTS (
       SELECT 1 FROM agent_profiles
        WHERE id <> NEW.id AND slug <> NEW.slug AND world_handle_key(slug) = world_handle_key(NEW.slug))
     OR EXISTS (
       SELECT 1 FROM agent_slug_aliases
        WHERE agent_id <> NEW.id AND world_handle_key(slug) = world_handle_key(NEW.slug)) THEN
    RAISE EXCEPTION '@% is taken', NEW.slug
      USING ERRCODE = 'unique_violation', CONSTRAINT = 'agent_handle_taken';
  END IF;
  RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS world_agent_handle_insert ON agent_profiles;
CREATE TRIGGER world_agent_handle_insert BEFORE INSERT ON agent_profiles
  FOR EACH ROW EXECUTE FUNCTION world_agent_handle_guard();

DROP TRIGGER IF EXISTS world_agent_handle_update ON agent_profiles;
CREATE TRIGGER world_agent_handle_update BEFORE UPDATE OF slug ON agent_profiles
  FOR EACH ROW WHEN (OLD.slug IS DISTINCT FROM NEW.slug) EXECUTE FUNCTION world_agent_handle_guard();

CREATE OR REPLACE FUNCTION world_agent_alias_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(hashtext('world_agent_handles'));
  IF EXISTS (
       SELECT 1 FROM agent_profiles
        WHERE id <> NEW.agent_id AND world_handle_key(slug) = world_handle_key(NEW.slug))
     OR EXISTS (
       SELECT 1 FROM agent_slug_aliases
        WHERE agent_id <> NEW.agent_id AND world_handle_key(slug) = world_handle_key(NEW.slug)) THEN
    RETURN NULL;
  END IF;
  RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS world_agent_alias_guard ON agent_slug_aliases;
CREATE TRIGGER world_agent_alias_guard BEFORE INSERT OR UPDATE ON agent_slug_aliases
  FOR EACH ROW EXECUTE FUNCTION world_agent_alias_guard();
