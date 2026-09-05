ALTER TABLE session_runs
  ADD COLUMN IF NOT EXISTS steered_into UUID REFERENCES session_runs(id) ON DELETE SET NULL,
  ADD COLUMN IF NOT EXISTS interrupt BOOLEAN NOT NULL DEFAULT false,
  ADD COLUMN IF NOT EXISTS stop_requested_by TEXT;

CREATE INDEX IF NOT EXISTS session_runs_queued_followups
  ON session_runs (session_id, created_at)
  WHERE kind = 'followup' AND status = 'queued';
