CREATE OR REPLACE FUNCTION check_parent_completion()
RETURNS TRIGGER AS $$
DECLARE
  parent_id UUID;
  parent RECORD;
  total_count INTEGER;
  finished_count INTEGER;
  done_count INTEGER;
  closed_id UUID;
  close_seq BIGINT;
  line JSONB;
BEGIN
  parent_id := NEW.parent_task_id;
  IF parent_id IS NULL THEN
    RETURN NEW;
  END IF;

  IF NEW.status NOT IN ('done', 'cancelled') THEN
    RETURN NEW;
  END IF;

  SELECT id, team_id, assigned_agent_id, title INTO parent FROM agent_tasks WHERE id = parent_id;

  SELECT COUNT(*), COUNT(*) FILTER (WHERE status IN ('done', 'cancelled')), COUNT(*) FILTER (WHERE status = 'done')
    INTO total_count, finished_count, done_count
    FROM agent_tasks
    WHERE parent_task_id = parent_id;

  IF parent.team_id IS NULL THEN
    IF total_count > 0 AND total_count = finished_count THEN
      UPDATE agent_tasks SET status = 'done'
       WHERE id = parent_id AND status NOT IN ('done', 'cancelled', 'failed')
       RETURNING id INTO closed_id;
      IF closed_id IS NOT NULL AND done_count > 0 AND parent.assigned_agent_id IS NOT NULL THEN
        line := jsonb_build_object(
          'task_id', parent_id, 'status', 'done', 'auto_closed', true, 'parts', total_count,
          'report', jsonb_build_object('summary', 'All ' || total_count || ' parts are finished, so the quest closed itself.'));
        INSERT INTO world_events (type, agent_id, task_id, payload)
          VALUES ('task.finished', parent.assigned_agent_id, parent_id, line)
          RETURNING seq INTO close_seq;
        INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
          VALUES (parent.assigned_agent_id, 'quest.parts', close_seq, jsonb_build_object(
            'task_id', parent_id, 'title', parent.title, 'closed', true, 'parts', total_count,
            'last_part', NEW.id, 'last_title', NEW.title));
      END IF;
    END IF;
    RETURN NEW;
  END IF;

  IF NEW.status <> 'done' OR TG_OP <> 'UPDATE' OR OLD.status = 'done' OR total_count = 0 OR done_count < total_count THEN
    RETURN NEW;
  END IF;

  UPDATE agent_tasks SET status = 'done'
   WHERE id = parent_id AND status NOT IN ('done', 'cancelled', 'failed')
   RETURNING id INTO closed_id;
  IF closed_id IS NULL THEN
    RETURN NEW;
  END IF;

  line := jsonb_build_object(
    'task_id', parent_id, 'status', 'done', 'auto_closed', true, 'parts', total_count,
    'report', jsonb_build_object('summary', 'All ' || total_count || ' parts are done, so the team quest closed itself.'));
  INSERT INTO world_events (type, agent_id, task_id, payload)
    VALUES ('task.finished', parent.assigned_agent_id, parent_id, line)
    RETURNING seq INTO close_seq;
  IF parent.assigned_agent_id IS NOT NULL THEN
    INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
      SELECT parent.assigned_agent_id, 'team.update', close_seq, jsonb_build_object(
        'team_id', tm.id, 'team', tm.name, 'channel', c.name, 'channel_id', tm.channel_id,
        'task_id', parent_id, 'title', parent.title, 'closed', true, 'parts', total_count,
        'last_part', NEW.id, 'last_title', NEW.title)
      FROM agent_teams tm LEFT JOIN channels c ON c.id = tm.channel_id
      WHERE tm.id = parent.team_id;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_task_assigned() RETURNS trigger AS $$
DECLARE
  new_seq BIGINT;
  body JSONB;
BEGIN
  IF NEW.assigned_agent_id IS NULL OR NEW.assignee_user_id IS NOT NULL THEN
    RETURN NEW;
  END IF;
  IF TG_OP = 'UPDATE' AND OLD.assigned_agent_id IS NOT DISTINCT FROM NEW.assigned_agent_id THEN
    RETURN NEW;
  END IF;
  body := jsonb_build_object('task_id', NEW.id, 'title', NEW.title, 'repo', NEW.repo);
  IF NEW.team_id IS NOT NULL THEN
    body := body || jsonb_build_object(
      'team_id', NEW.team_id,
      'team', (SELECT name FROM agent_teams WHERE id = NEW.team_id),
      'team_lead', (SELECT lead_agent_id = NEW.assigned_agent_id FROM agent_teams WHERE id = NEW.team_id),
      'part_of', NEW.parent_task_id,
      'part_of_title', (SELECT title FROM agent_tasks WHERE id = NEW.parent_task_id)
    );
  ELSIF NEW.parent_task_id IS NOT NULL THEN
    body := body || jsonb_build_object(
      'part_of', NEW.parent_task_id,
      'part_of_title', (SELECT title FROM agent_tasks WHERE id = NEW.parent_task_id),
      'part_from', (SELECT a.name FROM agent_tasks k JOIN agent_profiles a ON a.id = k.assigned_agent_id
                     WHERE k.id = NEW.parent_task_id AND k.assigned_agent_id IS DISTINCT FROM NEW.assigned_agent_id)
    );
  END IF;
  INSERT INTO world_events (type, agent_id, task_id, actor_user_id, payload)
    VALUES ('task.assigned', NEW.assigned_agent_id, NEW.id, NEW.user_id, body)
    RETURNING seq INTO new_seq;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
    VALUES (NEW.assigned_agent_id, 'task.assigned', new_seq, body);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_part_feed() RETURNS trigger AS $$
DECLARE
  part RECORD;
  part_agent UUID;
  who TEXT;
BEGIN
  IF NEW.task_id IS NULL OR NEW.type NOT IN ('task.finished', 'task.cancelled') THEN
    RETURN NEW;
  END IF;
  IF COALESCE((NEW.payload->>'quiet')::boolean, false) OR COALESCE((NEW.payload->>'auto_closed')::boolean, false) THEN
    RETURN NEW;
  END IF;
  SELECT k.title, k.assigned_agent_id, k.parent_task_id, p.title AS parent_title, p.status AS parent_status,
         p.assigned_agent_id AS parent_agent
    INTO part
    FROM agent_tasks k JOIN agent_tasks p ON p.id = k.parent_task_id
   WHERE k.id = NEW.task_id AND k.team_id IS NULL AND p.team_id IS NULL;
  IF NOT FOUND OR part.parent_agent IS NULL OR part.parent_status IN ('done', 'cancelled', 'failed') THEN
    RETURN NEW;
  END IF;
  part_agent := COALESCE(NEW.agent_id, part.assigned_agent_id);
  IF part_agent IS NOT DISTINCT FROM part.parent_agent THEN
    RETURN NEW;
  END IF;
  IF NEW.type = 'task.cancelled'
     AND NEW.payload->>'by' IS NOT DISTINCT FROM (SELECT name FROM agent_profiles WHERE id = part.parent_agent) THEN
    RETURN NEW;
  END IF;
  SELECT name INTO who FROM agent_profiles WHERE id = part_agent;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
    VALUES (part.parent_agent, 'quest.parts', NEW.seq, jsonb_build_object(
      'task_id', NEW.task_id, 'title', part.title, 'member', COALESCE(who, 'Someone'),
      'status', CASE WHEN NEW.type = 'task.cancelled' THEN 'cancelled'
                     ELSE COALESCE(NULLIF(NEW.payload->>'status', ''), 'done') END,
      'error', NEW.payload->>'error',
      'summary', left(COALESCE(NEW.payload->'report'->>'summary', ''), 400),
      'pr_url', NEW.payload->>'pr_url',
      'part_of', part.parent_task_id, 'part_of_title', part.parent_title));
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_part_feed ON world_events;
CREATE TRIGGER world_part_feed AFTER INSERT ON world_events
  FOR EACH ROW EXECUTE FUNCTION world_part_feed();
