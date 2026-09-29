ALTER TABLE session_runs
  ADD COLUMN IF NOT EXISTS cause TEXT,
  ADD COLUMN IF NOT EXISTS diagnosis JSONB,
  ADD COLUMN IF NOT EXISTS retry_cause TEXT,
  ADD COLUMN IF NOT EXISTS not_before TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_session_runs_retry_cause
  ON session_runs (task_id, retry_cause) WHERE retry_cause IS NOT NULL;
