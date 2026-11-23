ALTER TABLE agent_tasks ADD COLUMN merged_pr_url TEXT;

ALTER TABLE agent_tasks DISABLE TRIGGER check_parent_completion_trigger;
ALTER TABLE agent_tasks DISABLE TRIGGER update_agent_tasks_updated_at;

UPDATE agent_tasks t SET merged_pr_url = t.pr_url
WHERE t.pr_url IS NOT NULL AND EXISTS (
  SELECT 1 FROM world_events e
  WHERE e.task_id = t.id AND e.type = 'task.merged' AND e.payload->>'pr_url' = t.pr_url
);

ALTER TABLE agent_tasks ENABLE TRIGGER update_agent_tasks_updated_at;
ALTER TABLE agent_tasks ENABLE TRIGGER check_parent_completion_trigger;

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
      UPDATE agent_tasks SET status = 'done', error_message = NULL
       WHERE id = parent_id AND status NOT IN ('done', 'cancelled')
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

  UPDATE agent_tasks SET status = 'done', error_message = NULL
   WHERE id = parent_id AND status NOT IN ('done', 'cancelled')
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
