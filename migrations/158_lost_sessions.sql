ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS resumed_cli_id TEXT;
ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS retried_as UUID REFERENCES session_runs(id) ON DELETE SET NULL;

CREATE OR REPLACE FUNCTION world_note_resumed_cli_id() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'running' AND NEW.resumed_cli_id IS NULL
     AND (TG_OP = 'INSERT' OR OLD.status IS DISTINCT FROM 'running') THEN
    NEW.resumed_cli_id := COALESCE(
      (SELECT cli_session_id FROM agent_sessions WHERE id = NEW.fork_from),
      (SELECT cli_session_id FROM agent_sessions WHERE id = NEW.session_id),
      (SELECT p.cli_session_id FROM agent_sessions s JOIN agent_sessions p ON p.id = s.parent_session_id
        WHERE s.id = NEW.session_id AND s.role = 'quick'));
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_resumed_cli_id ON session_runs;
CREATE TRIGGER session_runs_resumed_cli_id BEFORE INSERT OR UPDATE OF status ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_note_resumed_cli_id();
