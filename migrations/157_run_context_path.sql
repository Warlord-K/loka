ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS context_path TEXT;
ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS context_note JSONB;
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'session_runs_context_path_check') THEN
    ALTER TABLE session_runs ADD CONSTRAINT session_runs_context_path_check
      CHECK (context_path IS NULL OR context_path IN ('native', 'checkpoint', 'recovery'));
  END IF;
END $$;
ALTER TABLE session_runs ADD COLUMN IF NOT EXISTS nudge TEXT;
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'session_runs_nudge_check') THEN
    ALTER TABLE session_runs ADD CONSTRAINT session_runs_nudge_check CHECK (nudge IS NULL OR nudge IN ('report', 'verdict'));
  END IF;
END $$;
UPDATE session_runs SET nudge = 'report' WHERE nudge IS NULL AND kind IN ('task', 'followup') AND starts_with(prompt, '[report missing]');
UPDATE session_runs SET nudge = 'verdict' WHERE nudge IS NULL AND kind = 'review' AND starts_with(prompt, '[verdict missing]');
