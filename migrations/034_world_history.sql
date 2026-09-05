DELETE FROM agent_profiles p
WHERE p.persona IS NULL
  AND p.owner_user_id IS NULL
  AND p.role = 'worker'
  AND NOT EXISTS (SELECT 1 FROM agent_sessions s WHERE s.agent_id = p.id)
  AND NOT EXISTS (SELECT 1 FROM agent_tasks t WHERE t.assigned_agent_id = p.id);

ALTER TABLE session_runs DROP CONSTRAINT IF EXISTS session_runs_kind_check;
ALTER TABLE session_runs ADD CONSTRAINT session_runs_kind_check
  CHECK (kind IN ('turn', 'task', 'review', 'aside', 'followup'));

CREATE TABLE IF NOT EXISTS session_run_events (
  id          BIGSERIAL PRIMARY KEY,
  run_id      UUID NOT NULL REFERENCES session_runs(id) ON DELETE CASCADE,
  kind        TEXT NOT NULL CHECK (kind IN ('tool', 'result', 'text', 'error')),
  tool        TEXT,
  body        TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_session_run_events_run ON session_run_events (run_id, id);

CREATE TABLE IF NOT EXISTS notification_reads (
  user_id        UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  last_seen_seq  BIGINT NOT NULL DEFAULT 0,
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
