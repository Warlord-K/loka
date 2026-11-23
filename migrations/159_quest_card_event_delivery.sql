CREATE INDEX IF NOT EXISTS idx_world_events_quest_card_tx
ON world_events (type, (payload->>'task_id'), created_at)
WHERE type IN ('task.changed', 'task.deleted');

CREATE OR REPLACE FUNCTION world_quest_changed(quest UUID, what TEXT) RETURNS void AS $$
DECLARE
  q RECORD;
BEGIN
  IF quest IS NULL THEN
    RETURN;
  END IF;
  IF EXISTS (
    SELECT 1 FROM world_events
    WHERE type = 'task.changed' AND payload->>'task_id' = quest::text AND created_at = transaction_timestamp()
  ) THEN
    RETURN;
  END IF;
  SELECT id, assigned_agent_id, user_id INTO q FROM agent_tasks WHERE id = quest;
  IF NOT FOUND THEN
    RETURN;
  END IF;
  INSERT INTO world_events (type, agent_id, task_id, actor_user_id, payload)
    VALUES ('task.changed', q.assigned_agent_id, q.id, q.user_id,
            jsonb_build_object('task_id', q.id, 'quiet', true));
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_task_deleted() RETURNS trigger AS $$
BEGIN
  IF OLD.org_id IS NULL OR NOT EXISTS (SELECT 1 FROM orgs WHERE id = OLD.org_id) THEN
    RETURN OLD;
  END IF;
  IF EXISTS (
    SELECT 1 FROM world_events
    WHERE type = 'task.deleted' AND payload->>'task_id' = OLD.id::text AND created_at = transaction_timestamp()
  ) THEN
    RETURN OLD;
  END IF;
  INSERT INTO world_events (type, agent_id, actor_user_id, org_id, payload)
    VALUES (
      'task.deleted',
      CASE WHEN EXISTS (SELECT 1 FROM agent_profiles WHERE id = OLD.assigned_agent_id) THEN OLD.assigned_agent_id END,
      CASE WHEN EXISTS (SELECT 1 FROM users WHERE id = OLD.user_id) THEN OLD.user_id END,
      OLD.org_id,
      jsonb_build_object('task_id', OLD.id, 'quiet', true)
    );
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_task_deleted ON agent_tasks;
CREATE TRIGGER world_task_deleted AFTER DELETE ON agent_tasks
FOR EACH ROW EXECUTE FUNCTION world_task_deleted();
