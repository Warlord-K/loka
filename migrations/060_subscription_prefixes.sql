CREATE OR REPLACE FUNCTION world_route_subscriptions() RETURNS trigger AS $$
DECLARE
  subject_name TEXT;
  subject_owner UUID;
  event_owner UUID;
  event_channel UUID;
  channel_public BOOLEAN := TRUE;
BEGIN
  IF COALESCE(NEW.payload->>'quiet', '') = 'true' OR NEW.payload->>'run_kind' = 'turn' THEN
    RETURN NEW;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM agent_subscriptions s
    WHERE s.event_type = NEW.type
       OR (right(s.event_type, 1) = '.' AND starts_with(NEW.type, s.event_type)
           AND NEW.type NOT IN ('agent.progress', 'run.activity'))
  ) THEN
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
  ELSIF NEW.payload ? 'owner_user_id' THEN
    event_owner := NULLIF(NEW.payload->>'owner_user_id', '')::uuid;
  END IF;
  IF starts_with(NEW.type, 'channel.') AND NEW.payload ? 'channel_id' THEN
    event_channel := NULLIF(NEW.payload->>'channel_id', '')::uuid;
    SELECT kind = 'channel' INTO channel_public FROM channels WHERE id = event_channel;
    channel_public := COALESCE(channel_public, FALSE);
  END IF;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
  SELECT DISTINCT ON (s.agent_id) s.agent_id, NEW.type, NEW.seq,
         NEW.payload || jsonb_build_object(
           'subject_agent_id', NEW.agent_id,
           'subject_agent', subject_name,
           'subscribed', true)
  FROM agent_subscriptions s
  JOIN agent_profiles p ON p.id = s.agent_id
  WHERE (s.event_type = NEW.type
         OR (right(s.event_type, 1) = '.' AND starts_with(NEW.type, s.event_type)
             AND NEW.type NOT IN ('agent.progress', 'run.activity')))
    AND s.agent_id IS DISTINCT FROM NEW.agent_id
    AND (NEW.agent_id IS NULL OR subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner)
    AND (event_owner IS NULL OR p.owner_user_id = event_owner)
    AND (event_channel IS NULL OR channel_public OR EXISTS (
      SELECT 1 FROM channel_members m
      WHERE m.channel_id = event_channel AND m.member_kind = 'agent' AND m.member_id = s.agent_id))
  ORDER BY s.agent_id;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
