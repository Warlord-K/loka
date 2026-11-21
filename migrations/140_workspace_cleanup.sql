ALTER TABLE world_runners ADD COLUMN IF NOT EXISTS clean_workspaces_at TIMESTAMPTZ;

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
    ELSIF NEW.type = 'runner.disk_low' THEN
        PERFORM world_bell_add(NULLIF(p->>'owner_user_id', '')::uuid, NEW, 'alert', p->>'runner_id');
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
