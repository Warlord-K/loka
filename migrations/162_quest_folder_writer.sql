CREATE INDEX IF NOT EXISTS agent_sessions_task_folder
  ON agent_sessions (working_dir)
  WHERE role = 'task' AND working_dir IS NOT NULL;

CREATE OR REPLACE FUNCTION world_notify_folder_free() RETURNS trigger AS $$
DECLARE
  folder TEXT;
  device UUID;
BEGIN
  SELECT working_dir, runner_id INTO folder, device
  FROM agent_sessions WHERE id = NEW.session_id AND role IN ('task', 'review');
  IF NULLIF(BTRIM(folder), '') IS NOT NULL AND EXISTS (
    SELECT 1 FROM session_runs waiting
    JOIN agent_sessions s ON s.id = waiting.session_id
    WHERE waiting.status = 'queued' AND s.role = 'task'
      AND s.working_dir = folder AND s.runner_id IS NOT DISTINCT FROM device
  ) THEN
    PERFORM pg_notify('session_runs', NEW.id::text);
    IF device IS NOT NULL THEN
      PERFORM pg_notify('runner_jobs', device::text);
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_folder_free ON session_runs;
CREATE TRIGGER session_runs_folder_free AFTER UPDATE OF status ON session_runs
  FOR EACH ROW
  WHEN (OLD.status = 'running' AND NEW.status <> 'running' AND NEW.kind IN ('task', 'followup', 'publish', 'review'))
  EXECUTE FUNCTION world_notify_folder_free();

CREATE OR REPLACE FUNCTION world_notify_lease_free() RETURNS trigger AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM session_runs WHERE runner_id = NEW.runner_id AND status = 'queued') THEN
    PERFORM pg_notify('runner_jobs', NEW.runner_id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS runner_leases_free ON runner_leases;
CREATE TRIGGER runner_leases_free AFTER UPDATE OF status ON runner_leases
  FOR EACH ROW
  WHEN (OLD.status IN ('active', 'finishing') AND NEW.status NOT IN ('active', 'finishing'))
  EXECUTE FUNCTION world_notify_lease_free();

ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS carries_on UUID REFERENCES session_runs(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS session_runs_carries_on ON session_runs (id) WHERE carries_on IS NOT NULL;
