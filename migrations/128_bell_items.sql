CREATE TABLE IF NOT EXISTS world_notifications (
    id          BIGSERIAL PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id      UUID REFERENCES orgs(id) ON DELETE CASCADE,
    event_seq   BIGINT NOT NULL REFERENCES world_events(seq) ON DELETE CASCADE,
    agent_id    UUID REFERENCES agent_profiles(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('ask', 'message', 'mention', 'reminder', 'quest', 'alert')),
    ref         TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    cleared_at  TIMESTAMPTZ,
    UNIQUE (user_id, event_seq)
);

CREATE INDEX IF NOT EXISTS idx_world_notifications_open ON world_notifications (user_id, org_id, id DESC) WHERE cleared_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_world_notifications_cleared ON world_notifications (user_id, org_id, cleared_at DESC) WHERE cleared_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_world_notifications_open_ref ON world_notifications (kind, ref) WHERE cleared_at IS NULL;

CREATE OR REPLACE FUNCTION world_bell_owner(agent UUID) RETURNS UUID
LANGUAGE sql STABLE AS $$
    SELECT owner_user_id FROM agent_profiles WHERE id = agent
$$;

CREATE OR REPLACE FUNCTION world_bell_add(person UUID, ev world_events, item_kind TEXT, item_ref TEXT) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    IF person IS NULL OR NOT EXISTS (SELECT 1 FROM users WHERE id = person) THEN
        RETURN;
    END IF;
    INSERT INTO world_notifications (user_id, org_id, event_seq, agent_id, kind, ref)
    VALUES (person, ev.org_id, ev.seq, ev.agent_id, item_kind, item_ref)
    ON CONFLICT (user_id, event_seq) DO NOTHING;
END;
$$;

CREATE OR REPLACE FUNCTION world_bell_route() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
DECLARE
    p JSONB := NEW.payload;
    mentioned TEXT;
BEGIN
    IF COALESCE(p->>'quiet', '') = 'true' THEN
        RETURN NULL;
    END IF;
    IF NEW.type = 'attention.requested' AND NOT p ? 'thread_user_id' AND NEW.agent_id IS NOT NULL THEN
        PERFORM world_bell_add(world_bell_owner(NEW.agent_id), NEW, 'ask', p->>'message_id');
    ELSIF NEW.type = 'message.created' AND NEW.agent_id IS NOT NULL
          AND p->>'author_kind' = 'agent' AND COALESCE(p->>'author_agent_id', '') = ''
          AND COALESCE(p->>'ask', '') <> 'true' THEN
        PERFORM world_bell_add(
            COALESCE(NULLIF(p->>'thread_user_id', '')::uuid, world_bell_owner(NEW.agent_id)),
            NEW, 'message', NEW.agent_id::text);
    ELSIF NEW.type = 'channel.message' AND jsonb_typeof(p->'mentions_users') = 'array' THEN
        FOR mentioned IN SELECT jsonb_array_elements_text(p->'mentions_users') LOOP
            IF mentioned IS DISTINCT FROM NEW.actor_user_id::text THEN
                PERFORM world_bell_add(mentioned::uuid, NEW, 'mention', p->>'channel_id');
            END IF;
        END LOOP;
    ELSIF NEW.type = 'todo.nudge' THEN
        PERFORM world_bell_add(NULLIF(p->>'owner_user_id', '')::uuid, NEW, 'reminder', p->>'todo_id');
    ELSIF NEW.type = 'timer.done' AND NOT (NEW.agent_id IS NOT NULL AND COALESCE(p->>'then', '') <> '') THEN
        PERFORM world_bell_add(NULLIF(p->>'owner_user_id', '')::uuid, NEW, 'reminder', NULL);
    ELSIF NEW.type = 'task.finished' AND NEW.agent_id IS NOT NULL AND COALESCE(p->>'error', '') <> '' THEN
        PERFORM world_bell_add(world_bell_owner(NEW.agent_id), NEW, 'quest', NEW.task_id::text);
    ELSIF NEW.type = 'org.budget' AND COALESCE(p->>'paused', '') = 'true' THEN
        PERFORM world_bell_add(m.user_id, NEW, 'alert', NULL)
           FROM org_members m WHERE m.org_id = NEW.org_id AND m.role IN ('owner', 'admin');
    ELSIF NEW.agent_id IS NOT NULL AND (
            (NEW.type = 'agent.budget' AND COALESCE(p->>'paused', '') = 'true')
         OR (NEW.type = 'agent.status' AND p->>'status' = 'error')) THEN
        PERFORM world_bell_add(world_bell_owner(NEW.agent_id), NEW, 'alert', NULL);
    END IF;
    RETURN NULL;
EXCEPTION WHEN invalid_text_representation THEN
    RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS world_bell_route ON world_events;
CREATE TRIGGER world_bell_route AFTER INSERT ON world_events
    FOR EACH ROW EXECUTE FUNCTION world_bell_route();

CREATE OR REPLACE FUNCTION world_bell_answered() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE world_notifications SET cleared_at = NOW()
     WHERE kind = 'ask' AND ref = NEW.meta->>'answers' AND cleared_at IS NULL;
    RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS world_bell_answered ON agent_messages;
CREATE TRIGGER world_bell_answered AFTER INSERT ON agent_messages
    FOR EACH ROW WHEN (NEW.meta ? 'answers') EXECUTE FUNCTION world_bell_answered();

INSERT INTO world_notifications (user_id, org_id, event_seq, agent_id, kind, ref, created_at)
SELECT a.owner_user_id, e.org_id, e.seq, e.agent_id, 'ask', e.payload->>'message_id', e.created_at
FROM world_events e
JOIN agent_profiles a ON a.id = e.agent_id
WHERE e.type = 'attention.requested'
  AND e.created_at > NOW() - INTERVAL '14 days'
  AND NOT e.payload ? 'thread_user_id'
  AND COALESCE(e.payload->>'quiet', '') <> 'true'
  AND a.owner_user_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM agent_messages m
       WHERE m.agent_id = e.agent_id AND m.meta->>'answers' = e.payload->>'message_id')
ON CONFLICT (user_id, event_seq) DO NOTHING;


DROP INDEX IF EXISTS idx_world_events_bell_agent;
DROP INDEX IF EXISTS idx_world_events_bell_replies;
DROP INDEX IF EXISTS idx_world_events_bell_unowned;
