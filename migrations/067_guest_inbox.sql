CREATE TABLE IF NOT EXISTS world_trusted (
  owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (owner_user_id, user_id),
  CHECK (owner_user_id <> user_id)
);

ALTER TABLE users ADD COLUMN IF NOT EXISTS mention_wakes_main BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE agent_inbox ADD COLUMN IF NOT EXISTS guest BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS idx_agent_inbox_guest_pending
  ON agent_inbox (agent_id) WHERE status = 'pending' AND guest;

CREATE OR REPLACE FUNCTION world_inbox_guest() RETURNS trigger AS $$
DECLARE
  owner UUID;
  sender UUID;
  author_agent UUID;
  is_owner BOOLEAN;
  is_trusted BOOLEAN;
BEGIN
  SELECT owner_user_id INTO owner FROM agent_profiles WHERE id = NEW.agent_id;
  IF NEW.event_seq IS NOT NULL THEN
    SELECT actor_user_id INTO sender FROM world_events WHERE seq = NEW.event_seq;
  END IF;
  IF sender IS NULL AND jsonb_typeof(NEW.payload) = 'object'
     AND NEW.payload->>'author_agent_id' ~ '^[0-9a-fA-F-]{36}$' THEN
    author_agent := (NEW.payload->>'author_agent_id')::uuid;
    SELECT owner_user_id INTO sender FROM agent_profiles WHERE id = author_agent;
  END IF;
  is_owner := sender IS NOT NULL AND owner IS NOT NULL AND sender = owner;
  is_trusted := sender IS NOT NULL AND owner IS NOT NULL AND NOT is_owner
    AND EXISTS (SELECT 1 FROM world_trusted t WHERE t.owner_user_id = owner AND t.user_id = sender);
  NEW.guest := sender IS NOT NULL AND owner IS NOT NULL AND NOT is_owner AND NOT is_trusted;
  IF jsonb_typeof(NEW.payload) = 'object' THEN
    NEW.payload := (NEW.payload - 'from_owner' - 'trusted')
      || jsonb_build_object(
           'from_owner', is_owner AND author_agent IS NULL,
           'trusted', is_trusted);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_inbox_guest ON agent_inbox;
CREATE TRIGGER agent_inbox_guest BEFORE INSERT ON agent_inbox
  FOR EACH ROW EXECUTE FUNCTION world_inbox_guest();
