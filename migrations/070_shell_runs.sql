CREATE TABLE IF NOT EXISTS shell_runs (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id        UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  owner_user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  runner_id       UUID REFERENCES world_runners(id) ON DELETE SET NULL,
  message_id      UUID REFERENCES agent_messages(id) ON DELETE SET NULL,
  command         TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'queued'
                  CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
  output          TEXT NOT NULL DEFAULT '',
  truncated       BOOLEAN NOT NULL DEFAULT FALSE,
  total_bytes     BIGINT NOT NULL DEFAULT 0,
  exit_code       INTEGER,
  duration_ms     BIGINT,
  error           TEXT,
  cancel_requested BOOLEAN NOT NULL DEFAULT FALSE,
  started_at      TIMESTAMPTZ,
  finished_at     TIMESTAMPTZ,
  heartbeat_at    TIMESTAMPTZ,
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS shell_runs_queue ON shell_runs (runner_id, created_at) WHERE status = 'queued';
CREATE INDEX IF NOT EXISTS shell_runs_agent ON shell_runs (agent_id, created_at DESC);
CREATE INDEX IF NOT EXISTS shell_runs_stale ON shell_runs (heartbeat_at) WHERE status = 'running';

CREATE OR REPLACE FUNCTION world_notify_shell_run() RETURNS trigger AS $$
BEGIN
  IF NEW.runner_id IS NOT NULL AND NEW.status = 'queued' THEN
    PERFORM pg_notify('runner_jobs', NEW.runner_id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS shell_runs_notify_runner ON shell_runs;
CREATE TRIGGER shell_runs_notify_runner AFTER INSERT ON shell_runs
  FOR EACH ROW EXECUTE FUNCTION world_notify_shell_run();
