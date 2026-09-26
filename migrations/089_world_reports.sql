CREATE TABLE IF NOT EXISTS world_reports (
  id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  author_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  author_user_id   UUID REFERENCES users(id) ON DELETE SET NULL,
  title            TEXT NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
  slug             TEXT NOT NULL,
  body             TEXT NOT NULL DEFAULT '' CHECK (octet_length(body) <= 524288),
  tags             TEXT[] NOT NULL DEFAULT '{}',
  folder           TEXT NOT NULL DEFAULT '',
  pinned           BOOLEAN NOT NULL DEFAULT FALSE,
  visibility       TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public')),
  links            UUID[] NOT NULL DEFAULT '{}',
  revision         INTEGER NOT NULL DEFAULT 1,
  task_id          UUID REFERENCES agent_tasks(id) ON DELETE SET NULL,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE (owner_user_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_world_reports_owner ON world_reports (owner_user_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_world_reports_public ON world_reports (updated_at DESC) WHERE visibility = 'public';
CREATE INDEX IF NOT EXISTS idx_world_reports_links ON world_reports USING GIN (links);
CREATE INDEX IF NOT EXISTS idx_world_reports_tags ON world_reports USING GIN (tags);

CREATE TABLE IF NOT EXISTS world_report_revisions (
  id               BIGSERIAL PRIMARY KEY,
  report_id        UUID NOT NULL REFERENCES world_reports(id) ON DELETE CASCADE,
  revision         INTEGER NOT NULL,
  title            TEXT NOT NULL,
  body             TEXT NOT NULL,
  folder           TEXT NOT NULL DEFAULT '',
  tags             TEXT[] NOT NULL DEFAULT '{}',
  author_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  author_user_id   UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE (report_id, revision)
);
