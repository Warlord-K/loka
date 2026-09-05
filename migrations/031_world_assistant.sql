ALTER TABLE session_runs DROP CONSTRAINT IF EXISTS session_runs_kind_check;
ALTER TABLE session_runs ADD CONSTRAINT session_runs_kind_check
  CHECK (kind IN ('turn', 'task', 'review', 'aside'));

ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS asked_by JSONB;

CREATE TABLE IF NOT EXISTS agent_subscriptions (
  agent_id    UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  event_type  TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (agent_id, event_type)
);

CREATE OR REPLACE FUNCTION world_route_subscriptions() RETURNS trigger AS $$
DECLARE
  subject_name TEXT;
  subject_owner UUID;
BEGIN
  IF NEW.agent_id IS NOT NULL THEN
    SELECT name, owner_user_id INTO subject_name, subject_owner FROM agent_profiles WHERE id = NEW.agent_id;
  END IF;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
  SELECT s.agent_id, NEW.type, NEW.seq,
         NEW.payload || jsonb_build_object(
           'subject_agent_id', NEW.agent_id,
           'subject_agent', subject_name,
           'subscribed', true)
  FROM agent_subscriptions s
  JOIN agent_profiles p ON p.id = s.agent_id
  WHERE s.event_type = NEW.type
    AND s.agent_id IS DISTINCT FROM NEW.agent_id
    AND (NEW.agent_id IS NULL OR subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_events_route ON world_events;
CREATE TRIGGER world_events_route AFTER INSERT ON world_events
  FOR EACH ROW EXECUTE FUNCTION world_route_subscriptions();
