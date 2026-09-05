ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS checkpoint TEXT;
ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS rewind JSONB;

CREATE TABLE IF NOT EXISTS task_milestones (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  task_id       UUID NOT NULL REFERENCES agent_tasks(id) ON DELETE CASCADE,
  position      INTEGER NOT NULL DEFAULT 0,
  title         TEXT NOT NULL,
  kind          TEXT NOT NULL DEFAULT 'work' CHECK (kind IN ('work', 'merge')),
  status        TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'active', 'done', 'failed', 'skipped')),
  detail        TEXT,
  data          JSONB NOT NULL DEFAULT '{}'::jsonb,
  next_check_at TIMESTAMPTZ,
  done_at       TIMESTAMPTZ,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS task_milestones_task ON task_milestones (task_id, position);
CREATE INDEX IF NOT EXISTS task_milestones_due ON task_milestones (next_check_at)
  WHERE kind = 'merge' AND status = 'active';
