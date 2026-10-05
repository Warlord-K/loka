CREATE OR REPLACE FUNCTION check_parent_completion()
RETURNS TRIGGER AS $$
DECLARE
  parent_id UUID;
  total_count INTEGER;
  finished_count INTEGER;
BEGIN
  parent_id := NEW.parent_task_id;
  IF parent_id IS NULL THEN
    RETURN NEW;
  END IF;

  IF NEW.status NOT IN ('done', 'cancelled') THEN
    RETURN NEW;
  END IF;

  SELECT COUNT(*), COUNT(*) FILTER (WHERE status IN ('done', 'cancelled'))
    INTO total_count, finished_count
    FROM agent_tasks
    WHERE parent_task_id = parent_id;

  IF total_count > 0 AND total_count = finished_count THEN
    UPDATE agent_tasks SET status = 'done'
     WHERE id = parent_id AND status NOT IN ('done', 'cancelled', 'failed');
  END IF;

  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
