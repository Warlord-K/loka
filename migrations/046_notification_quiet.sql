CREATE OR REPLACE FUNCTION world_route_subscriptions() RETURNS trigger AS $$
DECLARE
  subject_name TEXT;
  subject_owner UUID;
BEGIN
  IF COALESCE(NEW.payload->>'quiet', '') = 'true' OR NEW.payload->>'run_kind' = 'turn' THEN
    RETURN NEW;
  END IF;
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

UPDATE agent_inbox i
SET status = 'failed', error = 'dropped: main-session turn failure'
FROM agent_sessions s
WHERE i.status = 'pending'
  AND i.kind IN ('run.failed', 'run.timed_out')
  AND s.id::text = i.payload->>'session_id'
  AND s.role = 'main';

CREATE INDEX IF NOT EXISTS idx_world_events_agent_type_time ON world_events (agent_id, type, created_at DESC);
