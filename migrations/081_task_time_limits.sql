ALTER TABLE agent_tasks
  ADD COLUMN IF NOT EXISTS time_limit_secs INTEGER
  CHECK (time_limit_secs IS NULL OR time_limit_secs BETWEEN 300 AND 86400);

CREATE OR REPLACE FUNCTION world_apply_task_time_limit() RETURNS trigger AS $$
DECLARE
  quest UUID;
  limit_secs INTEGER;
BEGIN
  IF NEW.kind NOT IN ('task', 'followup', 'review') THEN
    RETURN NEW;
  END IF;
  quest := NEW.task_id;
  IF quest IS NULL THEN
    SELECT task_id INTO quest FROM agent_sessions WHERE id = NEW.session_id;
  END IF;
  IF quest IS NOT NULL THEN
    SELECT time_limit_secs INTO limit_secs FROM agent_tasks WHERE id = quest;
    IF limit_secs IS NOT NULL THEN
      NEW.timeout_secs := limit_secs;
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_apply_task_time_limit ON session_runs;
CREATE TRIGGER world_apply_task_time_limit
  BEFORE INSERT ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_apply_task_time_limit();
