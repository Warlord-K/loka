ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS waiting_for TEXT CHECK (waiting_for IN ('disk'));

CREATE INDEX IF NOT EXISTS idx_session_runs_waiting ON session_runs (runner_id) WHERE status = 'queued' AND waiting_for IS NOT NULL;
