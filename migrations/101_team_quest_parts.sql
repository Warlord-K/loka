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
       WHERE id = parent_id AND status NOT IN ('done', 'cancelled', 'failed');
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
  END IF;
  INSERT INTO world_events (type, agent_id, task_id, actor_user_id, payload)
    VALUES ('task.assigned', NEW.assigned_agent_id, NEW.id, NEW.user_id, body)
    RETURNING seq INTO new_seq;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
    VALUES (NEW.assigned_agent_id, 'task.assigned', new_seq, body);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_team_feed() RETURNS trigger AS $$
DECLARE
  team RECORD;
  who TEXT;
  line TEXT;
  status TEXT;
  summary TEXT;
  msg_id BIGINT;
  feed_seq BIGINT;
  parent_status TEXT;
BEGIN
  IF NEW.task_id IS NULL OR NEW.type NOT IN ('task.finished', 'task.merged', 'task.cancelled') THEN
    RETURN NEW;
  END IF;
  IF COALESCE((NEW.payload->>'quiet')::boolean, false) THEN
    RETURN NEW;
  END IF;
  SELECT tm.id, tm.name, tm.owner_user_id, tm.lead_agent_id, tm.channel_id, c.name AS channel_name,
         k.title, k.assigned_agent_id, k.parent_task_id, p.title AS parent_title
    INTO team
    FROM agent_tasks k
    JOIN agent_teams tm ON tm.id = k.team_id
    LEFT JOIN channels c ON c.id = tm.channel_id
    LEFT JOIN agent_tasks p ON p.id = k.parent_task_id
   WHERE k.id = NEW.task_id;
  IF NOT FOUND OR team.channel_id IS NULL THEN
    RETURN NEW;
  END IF;
  IF team.parent_task_id IS NOT NULL THEN
    SELECT p.status INTO parent_status FROM agent_tasks p WHERE p.id = team.parent_task_id;
  END IF;
  SELECT name INTO who FROM agent_profiles WHERE id = COALESCE(NEW.agent_id, team.assigned_agent_id);
  who := COALESCE(who, 'Someone');
  status := COALESCE(NULLIF(NEW.payload->>'status', ''), 'done');
  summary := left(COALESCE(NEW.payload->'report'->>'summary', ''), 400);
  line := CASE
    WHEN COALESCE((NEW.payload->>'auto_closed')::boolean, false) THEN
      'All ' || COALESCE(NEW.payload->>'parts', '') || ' parts of quest:' || NEW.task_id || ' are done, so the team quest closed itself.'
    WHEN NEW.type = 'task.finished' THEN
      CASE WHEN COALESCE(NEW.payload->>'error', '') <> '' THEN
        who || ' could not finish quest:' || NEW.task_id || E'\n' || left(NEW.payload->>'error', 300)
      ELSE
        who || ' finished quest:' || NEW.task_id || ' (' || status || ')'
          || CASE WHEN summary <> '' THEN E'\n' || summary ELSE '' END
          || CASE WHEN COALESCE(NEW.payload->>'pr_url', '') <> '' THEN E'\n' || (NEW.payload->>'pr_url') ELSE '' END
      END
    WHEN NEW.type = 'task.merged' THEN 'The pull request for quest:' || NEW.task_id || ' merged.'
    ELSE 'quest:' || NEW.task_id || ' was cancelled' || COALESCE(' by ' || NULLIF(NEW.payload->>'by', ''), '') || '.'
  END;
  INSERT INTO channel_messages (channel_id, author_kind, author_agent_id, body, meta)
    VALUES (team.channel_id, 'system', NEW.agent_id, line,
            jsonb_build_object('kind', 'team_feed', 'team_id', team.id, 'task_id', NEW.task_id, 'event', NEW.type))
    RETURNING id INTO msg_id;
  INSERT INTO world_events (type, agent_id, task_id, payload)
    VALUES ('channel.message', NEW.agent_id, NEW.task_id, jsonb_build_object(
      'channel_id', team.channel_id, 'channel', '#' || team.channel_name, 'message_id', msg_id,
      'author', who, 'author_kind', 'system', 'author_agent_id', NEW.agent_id, 'body', line,
      'team_id', team.id, 'quiet', true))
    RETURNING seq INTO feed_seq;
  IF NEW.type IN ('task.finished', 'task.cancelled') AND team.lead_agent_id IS NOT NULL
     AND NOT COALESCE((NEW.payload->>'auto_closed')::boolean, false)
     AND parent_status IS DISTINCT FROM 'done'
     AND (CASE WHEN NEW.type = 'task.cancelled'
               THEN NEW.payload->>'by' IS DISTINCT FROM (SELECT name FROM agent_profiles WHERE id = team.lead_agent_id)
               ELSE team.lead_agent_id IS DISTINCT FROM COALESCE(NEW.agent_id, team.assigned_agent_id) END)
     AND (NEW.type = 'task.finished' OR team.parent_task_id IS NOT NULL) THEN
    INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
      VALUES (team.lead_agent_id, 'team.update', feed_seq, jsonb_build_object(
        'team_id', team.id, 'team', team.name, 'channel', team.channel_name, 'channel_id', team.channel_id,
        'task_id', NEW.task_id, 'title', team.title, 'member', who,
        'status', CASE WHEN NEW.type = 'task.cancelled' THEN 'cancelled' ELSE status END,
        'error', NEW.payload->>'error', 'summary', summary, 'pr_url', NEW.payload->>'pr_url',
        'part_of', team.parent_task_id, 'part_of_title', team.parent_title));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
