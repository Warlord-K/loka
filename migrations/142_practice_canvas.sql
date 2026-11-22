ALTER TABLE world_reports ADD COLUMN IF NOT EXISTS practice JSONB;
ALTER TABLE world_reports DROP CONSTRAINT IF EXISTS world_reports_practice_shape;
ALTER TABLE world_reports ADD CONSTRAINT world_reports_practice_shape CHECK (
  practice IS NULL OR (
    kind = 'canvas'
    AND jsonb_typeof(practice) = 'object'
    AND jsonb_typeof(practice->'subject') = 'string'
    AND length(practice->>'subject') BETWEEN 1 AND 40
    AND (practice->'coach_agent_id' IS NULL OR jsonb_typeof(practice->'coach_agent_id') IN ('string', 'null'))
  )
);
CREATE INDEX IF NOT EXISTS idx_world_reports_practice ON world_reports (owner_user_id, (practice->>'subject'), updated_at DESC) WHERE practice IS NOT NULL;

CREATE TABLE IF NOT EXISTS world_practice_attempts (
  id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  report_id        UUID NOT NULL REFERENCES world_reports(id) ON DELETE CASCADE,
  user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  subject          TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 40),
  coach_agent_id   UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  n                INT NOT NULL CHECK (n >= 1),
  image_upload_id  UUID REFERENCES world_uploads(id) ON DELETE SET NULL,
  strokes          JSONB NOT NULL CHECK (octet_length(strokes::text) <= 8388608),
  note             TEXT CHECK (note IS NULL OR length(note) <= 2000),
  status           TEXT NOT NULL DEFAULT 'waiting' CHECK (status IN ('waiting', 'checked')),
  "check"          JSONB,
  coach_message_id UUID REFERENCES agent_messages(id) ON DELETE SET NULL,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  checked_at       TIMESTAMPTZ,
  UNIQUE (report_id, n),
  CHECK ((status = 'checked') = ("check" IS NOT NULL AND checked_at IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS idx_world_practice_attempts_report ON world_practice_attempts (report_id, n DESC);
CREATE INDEX IF NOT EXISTS idx_world_practice_attempts_user ON world_practice_attempts (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_world_practice_attempts_image ON world_practice_attempts (image_upload_id) WHERE image_upload_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_world_practice_attempts_waiting ON world_practice_attempts (coach_agent_id) WHERE status = 'waiting';

CREATE TABLE IF NOT EXISTS world_practice_coach_prefs (
  user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  subject    TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 40),
  agent_id   UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, subject)
);
