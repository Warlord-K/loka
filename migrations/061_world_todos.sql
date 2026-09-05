CREATE TABLE IF NOT EXISTS world_todos (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  text            TEXT NOT NULL,
  priority        SMALLINT NOT NULL DEFAULT 2 CHECK (priority BETWEEN 0 AND 3),
  position        DOUBLE PRECISION,
  added_by_agent  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  added_by_user   UUID REFERENCES users(id) ON DELETE SET NULL,
  source          JSONB,
  due_at          TIMESTAMPTZ,
  nudge_at        TIMESTAMPTZ,
  nudge_count     INTEGER NOT NULL DEFAULT 0,
  repeat_nudge    BOOLEAN NOT NULL DEFAULT TRUE,
  last_nudged_at  TIMESTAMPTZ,
  status          TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'done', 'dismissed')),
  done_at         TIMESTAMPTZ,
  added_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_world_todos_owner ON world_todos (owner_user_id, status);
CREATE INDEX IF NOT EXISTS idx_world_todos_nudge ON world_todos (nudge_at) WHERE status = 'open' AND nudge_at IS NOT NULL;

CREATE OR REPLACE FUNCTION world_notify_todo_nudge() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('schedules_changed', NEW.id::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_todos_nudge_notify ON world_todos;
CREATE TRIGGER world_todos_nudge_notify AFTER INSERT OR UPDATE OF nudge_at, status ON world_todos
  FOR EACH ROW EXECUTE FUNCTION world_notify_todo_nudge();
