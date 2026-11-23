ALTER TABLE world_rules ADD COLUMN IF NOT EXISTS github_repos TEXT[] NOT NULL DEFAULT '{}';

UPDATE world_rules r
SET github_repos = ARRAY[named.repo]
FROM (
  SELECT w.id, MIN(m[1]) AS repo, COUNT(DISTINCT lower(m[1])) AS repos
  FROM world_rules w,
       regexp_matches(w.body, '(?<![A-Za-z0-9._/-])([A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9._-]*[A-Za-z0-9_-])(?![A-Za-z0-9_/-])', 'g') AS m
  WHERE w.scope = 'allow' AND w.body ILIKE '%--match-head-commit%'
  GROUP BY w.id
) named
WHERE r.id = named.id AND named.repos = 1 AND r.github_repos = '{}';

ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS published_head TEXT;
ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS published_head_at TIMESTAMPTZ;

ALTER TABLE agent_sessions DROP CONSTRAINT IF EXISTS agent_sessions_role_check;
ALTER TABLE agent_sessions ADD CONSTRAINT agent_sessions_role_check
  CHECK (role IN ('main', 'task', 'aside', 'review', 'designer', 'direct', 'quick', 'action'));

ALTER TABLE session_runs DROP CONSTRAINT IF EXISTS session_runs_kind_check;
ALTER TABLE session_runs ADD CONSTRAINT session_runs_kind_check
  CHECK (kind IN ('turn', 'task', 'review', 'aside', 'followup', 'publish', 'direct', 'quick', 'action'));

ALTER TABLE schedules ADD COLUMN IF NOT EXISTS action JSONB;
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS on_event TEXT;
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS fired JSONB;
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS outcome JSONB;
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS run_id UUID REFERENCES session_runs(id) ON DELETE SET NULL;

ALTER TABLE schedules DROP CONSTRAINT IF EXISTS schedules_kind_check;
ALTER TABLE schedules ADD CONSTRAINT schedules_kind_check CHECK (kind IN ('routine', 'heartbeat', 'action'));

ALTER TABLE schedules DROP CONSTRAINT IF EXISTS schedules_on_event_check;
ALTER TABLE schedules ADD CONSTRAINT schedules_on_event_check
  CHECK (on_event IS NULL OR on_event IN ('task.finished', 'task.merged', 'task.pr_closed'));
ALTER TABLE schedules DROP CONSTRAINT IF EXISTS schedules_action_check;
ALTER TABLE schedules ADD CONSTRAINT schedules_action_check
  CHECK ((kind = 'action') = (action IS NOT NULL) AND (kind <> 'action' OR (task_id IS NOT NULL AND cron_expression IS NULL)));

CREATE INDEX IF NOT EXISTS idx_schedules_waiting_actions ON schedules (task_id)
  WHERE kind = 'action' AND on_event IS NOT NULL AND fired IS NULL AND outcome IS NULL;

CREATE OR REPLACE FUNCTION world_end_quest_schedules() RETURNS trigger AS $$
BEGIN
  IF NEW.status IN ('done', 'failed', 'cancelled') AND OLD.status IS DISTINCT FROM NEW.status THEN
    DELETE FROM schedules WHERE task_id = NEW.id AND kind <> 'action';
  END IF;
  IF NEW.status = 'cancelled' AND OLD.status IS DISTINCT FROM NEW.status THEN
    UPDATE schedules
    SET fired = jsonb_build_object('type', 'task.cancelled', 'at', NOW()), next_run_at = NOW(), enabled = TRUE
    WHERE task_id = NEW.id AND kind = 'action' AND outcome IS NULL AND fired IS NULL;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_fire_pr_actions() RETURNS trigger AS $$
BEGIN
  IF NEW.task_id IS NULL THEN
    RETURN NULL;
  END IF;
  IF NEW.type = 'review.finished' AND EXISTS (
       SELECT 1 FROM agent_sessions s WHERE s.id = NEW.session_id AND s.asked_by ? 'compare') THEN
    RETURN NULL;
  END IF;
  IF NEW.type IN ('task.finished', 'review.finished', 'task.merged', 'task.pr_closed') THEN
    UPDATE schedules
    SET fired = jsonb_build_object('type', NEW.type, 'seq', NEW.seq, 'at', NOW()), next_run_at = NOW()
    WHERE task_id = NEW.task_id AND kind = 'action' AND enabled
      AND on_event IS NOT NULL AND fired IS NULL AND outcome IS NULL
      AND (on_event = NEW.type
           OR (on_event = 'task.finished' AND NEW.type = 'review.finished')
           OR NEW.type IN ('task.merged', 'task.pr_closed'));
  END IF;
  RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_fire_pr_actions ON world_events;
CREATE TRIGGER world_fire_pr_actions AFTER INSERT ON world_events
  FOR EACH ROW EXECUTE FUNCTION world_fire_pr_actions();

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
    ELSIF NEW.type = 'action.ran' AND NEW.agent_id IS NOT NULL THEN
        PERFORM world_bell_add(world_bell_owner(NEW.agent_id), NEW, 'quest', NEW.task_id::text);
    ELSIF NEW.type IN ('action.skipped', 'action.failed') AND NEW.agent_id IS NOT NULL THEN
        PERFORM world_bell_add(world_bell_owner(NEW.agent_id), NEW, 'alert', NEW.task_id::text);
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
