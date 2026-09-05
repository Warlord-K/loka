CREATE TABLE IF NOT EXISTS terminal_sessions (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  runner_id       UUID NOT NULL REFERENCES world_runners(id) ON DELETE CASCADE,
  cols            INTEGER NOT NULL DEFAULT 80,
  rows            INTEGER NOT NULL DEFAULT 24,
  status          TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'open', 'closed')),
  close_reason    TEXT,
  last_activity_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  closed_at       TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS terminal_sessions_live ON terminal_sessions (runner_id) WHERE closed_at IS NULL;
CREATE INDEX IF NOT EXISTS terminal_sessions_owner ON terminal_sessions (owner_user_id, created_at DESC);

CREATE OR REPLACE FUNCTION world_notify_terminal_session() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'pending' AND NEW.closed_at IS NULL THEN
    PERFORM pg_notify('runner_jobs', NEW.runner_id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS terminal_sessions_notify_runner ON terminal_sessions;
CREATE TRIGGER terminal_sessions_notify_runner AFTER INSERT ON terminal_sessions
  FOR EACH ROW EXECUTE FUNCTION world_notify_terminal_session();
