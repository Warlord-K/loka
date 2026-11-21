ALTER TABLE world_reports ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'doc' CHECK (kind IN ('doc', 'canvas'));
ALTER TABLE world_reports ADD COLUMN IF NOT EXISTS canvas JSONB CHECK (canvas IS NULL OR octet_length(canvas::text) <= 524288);
ALTER TABLE world_report_revisions ADD COLUMN IF NOT EXISTS canvas JSONB;

CREATE TABLE IF NOT EXISTS world_canvas_marks (
  id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  report_id            UUID NOT NULL REFERENCES world_reports(id) ON DELETE CASCADE,
  item_id              TEXT CHECK (item_id IS NULL OR length(item_id) <= 40),
  x                    DOUBLE PRECISION NOT NULL DEFAULT 0,
  y                    DOUBLE PRECISION NOT NULL DEFAULT 0,
  kind                 TEXT NOT NULL DEFAULT 'pin' CHECK (kind IN ('pin', 'sticky')),
  body                 TEXT NOT NULL CHECK (length(btrim(body)) BETWEEN 1 AND 2000),
  status               TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
  reply                TEXT CHECK (reply IS NULL OR length(reply) <= 2000),
  author_user_id       UUID REFERENCES users(id) ON DELETE SET NULL,
  resolved_by_agent_id UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  resolved_by_user_id  UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  resolved_at          TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_world_canvas_marks_report ON world_canvas_marks (report_id, created_at);
CREATE INDEX IF NOT EXISTS idx_world_canvas_marks_open ON world_canvas_marks (report_id) WHERE status = 'open';
