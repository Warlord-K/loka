ALTER TABLE agent_tasks
  ADD COLUMN IF NOT EXISTS plan_first BOOLEAN NOT NULL DEFAULT FALSE,
  ADD COLUMN IF NOT EXISTS attempts SMALLINT NOT NULL DEFAULT 1 CHECK (attempts BETWEEN 1 AND 3),
  ADD COLUMN IF NOT EXISTS attempt_variants JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_plan_or_attempts;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_plan_or_attempts CHECK (NOT (plan_first AND attempts > 1));

ALTER TABLE agent_sessions
  ADD COLUMN IF NOT EXISTS plan_state TEXT CHECK (plan_state IN ('planning', 'awaiting', 'approved')),
  ADD COLUMN IF NOT EXISTS attempt SMALLINT CHECK (attempt BETWEEN 1 AND 3),
  ADD COLUMN IF NOT EXISTS attempt_state TEXT CHECK (attempt_state IN ('running', 'finished', 'failed', 'won', 'lost'));

CREATE INDEX IF NOT EXISTS idx_agent_sessions_attempts ON agent_sessions (task_id) WHERE attempt IS NOT NULL;

CREATE TABLE IF NOT EXISTS quest_ci_feedback (
  task_id    UUID NOT NULL REFERENCES agent_tasks(id) ON DELETE CASCADE,
  head_sha   TEXT NOT NULL,
  run_id     UUID,
  jobs       JSONB NOT NULL DEFAULT '[]'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (task_id, head_sha)
);
