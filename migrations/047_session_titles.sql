ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS title TEXT;

CREATE OR REPLACE FUNCTION world_session_title(p_role TEXT, p_agent_id UUID, p_task_id UUID, p_asked_by JSONB) RETURNS TEXT AS $$
DECLARE
  task_title TEXT;
  agent_name TEXT;
  question TEXT;
BEGIN
  IF p_task_id IS NOT NULL THEN
    SELECT title INTO task_title FROM agent_tasks WHERE id = p_task_id;
  END IF;
  CASE p_role
    WHEN 'task' THEN
      RETURN task_title;
    WHEN 'review' THEN
      RETURN CASE WHEN task_title IS NULL THEN 'Review' ELSE 'Review: ' || task_title END;
    WHEN 'aside' THEN
      question := btrim(regexp_replace(COALESCE(p_asked_by->>'question', ''), '\s+', ' ', 'g'));
      IF question = '' THEN
        RETURN 'Aside';
      END IF;
      RETURN CASE WHEN char_length(question) > 60 THEN rtrim(left(question, 57)) || '...' ELSE question END;
    WHEN 'main' THEN
      SELECT name INTO agent_name FROM agent_profiles WHERE id = p_agent_id;
      RETURN COALESCE(agent_name, 'Agent') || ' main';
    ELSE
      RETURN NULL;
  END CASE;
END;
$$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION world_session_set_title() RETURNS trigger AS $$
BEGIN
  IF NEW.title IS NULL OR (TG_OP = 'UPDATE' AND NEW.task_id IS DISTINCT FROM OLD.task_id AND NEW.title IS NOT DISTINCT FROM OLD.title) THEN
    NEW.title := world_session_title(NEW.role, NEW.agent_id, NEW.task_id, NEW.asked_by);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_session_title ON agent_sessions;
CREATE TRIGGER world_session_title
  BEFORE INSERT OR UPDATE OF task_id, title ON agent_sessions
  FOR EACH ROW EXECUTE FUNCTION world_session_set_title();

UPDATE agent_sessions
SET title = world_session_title(role, agent_id, task_id, asked_by)
WHERE title IS NULL;
