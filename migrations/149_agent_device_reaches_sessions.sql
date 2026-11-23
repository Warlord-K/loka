CREATE OR REPLACE FUNCTION world_hand_device_to_sessions(agent UUID) RETURNS INTEGER AS $$
DECLARE
  device UUID;
  freed UUID[];
BEGIN
  SELECT runner_id INTO device FROM agent_profiles WHERE id = agent;
  IF device IS NULL THEN
    RETURN 0;
  END IF;
  WITH moved AS (
    UPDATE agent_sessions SET runner_id = device
    WHERE agent_id = agent AND runner_id IS NULL AND cli_session_id IS NULL
      AND status <> 'archived' AND role <> 'direct'
    RETURNING id
  ), woken AS (
    UPDATE session_runs SET runner_id = device
    WHERE status = 'queued' AND runner_id IS NULL AND session_id IN (SELECT id FROM moved)
    RETURNING id
  )
  SELECT ARRAY(SELECT id FROM moved) INTO freed;
  IF cardinality(freed) > 0 THEN
    PERFORM pg_notify('runner_jobs', device::text);
  END IF;
  RETURN cardinality(freed);
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_agent_device_reaches_sessions() RETURNS trigger AS $$
BEGIN
  PERFORM world_hand_device_to_sessions(NEW.id);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_profiles_device_reaches_sessions ON agent_profiles;
CREATE TRIGGER agent_profiles_device_reaches_sessions AFTER UPDATE OF runner_id ON agent_profiles
  FOR EACH ROW WHEN (OLD.runner_id IS DISTINCT FROM NEW.runner_id AND NEW.runner_id IS NOT NULL)
  EXECUTE FUNCTION world_agent_device_reaches_sessions();

CREATE OR REPLACE FUNCTION world_main_session_workspace() RETURNS trigger AS $$
BEGIN
  IF NEW.role = 'main' AND NULLIF(BTRIM(NEW.working_dir), '') IS NULL THEN
    NEW.working_dir := 'agents/' || NEW.agent_id::text;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_sessions_main_workspace ON agent_sessions;
CREATE TRIGGER agent_sessions_main_workspace BEFORE INSERT ON agent_sessions
  FOR EACH ROW EXECUTE FUNCTION world_main_session_workspace();

UPDATE agent_sessions SET working_dir = 'agents/' || agent_id::text
WHERE role = 'main' AND NULLIF(BTRIM(working_dir), '') IS NULL;

SELECT world_hand_device_to_sessions(id) FROM agent_profiles a
WHERE runner_id IS NOT NULL
  AND EXISTS (
    SELECT 1 FROM agent_sessions s
    WHERE s.agent_id = a.id AND s.runner_id IS NULL AND s.cli_session_id IS NULL
      AND s.status <> 'archived' AND s.role <> 'direct');
