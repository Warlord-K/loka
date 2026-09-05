CREATE TABLE IF NOT EXISTS slack_seen (
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  team_id     TEXT NOT NULL,
  channel_id  TEXT NOT NULL,
  ts          TEXT NOT NULL,
  event_seq   BIGINT,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, team_id, channel_id, ts)
);

CREATE INDEX IF NOT EXISTS slack_seen_created_at ON slack_seen (created_at);

CREATE OR REPLACE FUNCTION world_route_subscriptions() RETURNS trigger AS $$
DECLARE
  subject_name TEXT;
  subject_owner UUID;
  event_owner UUID;
BEGIN
  IF COALESCE(NEW.payload->>'quiet', '') = 'true' OR NEW.payload->>'run_kind' = 'turn' THEN
    RETURN NEW;
  END IF;
  IF NEW.agent_id IS NOT NULL THEN
    SELECT name, owner_user_id INTO subject_name, subject_owner FROM agent_profiles WHERE id = NEW.agent_id;
  END IF;
  IF NEW.type = 'slack.notification' THEN
    event_owner := NULLIF(NEW.payload->>'owner_user_id', '')::uuid;
    IF event_owner IS NULL THEN
      RETURN NEW;
    END IF;
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
    AND (NEW.agent_id IS NULL OR subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner)
    AND (event_owner IS NULL OR p.owner_user_id = event_owner);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
