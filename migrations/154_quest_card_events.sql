CREATE OR REPLACE FUNCTION world_quest_changed(quest UUID, what TEXT) RETURNS void AS $$
DECLARE
  q RECORD;
BEGIN
  IF quest IS NULL THEN
    RETURN;
  END IF;
  IF EXISTS (SELECT 1 FROM world_events WHERE task_id = quest AND type = 'task.changed' AND created_at = NOW()) THEN
    RETURN;
  END IF;
  SELECT id, status, title, assigned_agent_id, user_id INTO q FROM agent_tasks WHERE id = quest;
  IF NOT FOUND THEN
    RETURN;
  END IF;
  INSERT INTO world_events (type, agent_id, task_id, actor_user_id, payload)
    VALUES ('task.changed', q.assigned_agent_id, q.id, q.user_id,
            jsonb_build_object('task_id', q.id, 'status', q.status, 'title', q.title, 'changed', what, 'quiet', true));
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_task_card_changed() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND (NEW.status, NEW.title, NEW.repo, NEW.pr_url, NEW.result_content, NEW.assigned_agent_id, NEW.assignee_user_id, NEW.plan_first, NEW.attempts, NEW.pace)
       IS NOT DISTINCT FROM (OLD.status, OLD.title, OLD.repo, OLD.pr_url, OLD.result_content, OLD.assigned_agent_id, OLD.assignee_user_id, OLD.plan_first, OLD.attempts, OLD.pace) THEN
    RETURN NEW;
  END IF;
  PERFORM world_quest_changed(NEW.id, CASE WHEN TG_OP = 'INSERT' THEN 'created' WHEN NEW.status IS DISTINCT FROM OLD.status THEN 'status' ELSE 'details' END);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_task_status_event ON agent_tasks;
DROP FUNCTION IF EXISTS world_task_status_event();
DROP TRIGGER IF EXISTS world_task_card_changed ON agent_tasks;
CREATE TRIGGER world_task_card_changed AFTER INSERT OR UPDATE ON agent_tasks
FOR EACH ROW EXECUTE FUNCTION world_task_card_changed();

CREATE OR REPLACE FUNCTION world_quest_plan_changed() RETURNS trigger AS $$
BEGIN
  IF NEW.task_id IS NULL
     OR ((NEW.plan_state = 'awaiting' AND NEW.status <> 'archived') IS NOT DISTINCT FROM (OLD.plan_state = 'awaiting' AND OLD.status <> 'archived')) THEN
    RETURN NEW;
  END IF;
  PERFORM world_quest_changed(NEW.task_id, 'plan');
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_quest_plan_changed ON agent_sessions;
CREATE TRIGGER world_quest_plan_changed AFTER UPDATE OF plan_state, status ON agent_sessions
FOR EACH ROW EXECUTE FUNCTION world_quest_plan_changed();

CREATE OR REPLACE FUNCTION world_quest_milestones_changed() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    PERFORM world_quest_changed(OLD.task_id, 'milestones');
    RETURN OLD;
  END IF;
  IF TG_OP = 'UPDATE' AND (NEW.title, NEW.status, NEW.position, NEW.kind) IS NOT DISTINCT FROM (OLD.title, OLD.status, OLD.position, OLD.kind) THEN
    RETURN NEW;
  END IF;
  PERFORM world_quest_changed(NEW.task_id, 'milestones');
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_quest_milestones_changed ON task_milestones;
CREATE TRIGGER world_quest_milestones_changed AFTER INSERT OR UPDATE OR DELETE ON task_milestones
FOR EACH ROW EXECUTE FUNCTION world_quest_milestones_changed();
