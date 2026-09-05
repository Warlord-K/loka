ALTER TABLE users ADD COLUMN IF NOT EXISTS slack_source TEXT NOT NULL DEFAULT 'both';
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_slack_source_check;
ALTER TABLE users ADD CONSTRAINT users_slack_source_check CHECK (slack_source IN ('desktop', 'app', 'both'));

CREATE TABLE IF NOT EXISTS slack_identities (
  team_id        TEXT NOT NULL,
  slack_user_id  TEXT NOT NULL,
  user_id        UUID REFERENCES users(id) ON DELETE SET NULL,
  linked_by      TEXT NOT NULL DEFAULT 'none' CHECK (linked_by IN ('none', 'email', 'code', 'unlinked')),
  name           TEXT NOT NULL DEFAULT '',
  email          TEXT,
  hinted_at      TIMESTAMPTZ,
  fetched_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (team_id, slack_user_id)
);

CREATE INDEX IF NOT EXISTS slack_identities_user ON slack_identities (user_id) WHERE user_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS slack_link_codes (
  code        TEXT PRIMARY KEY,
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS slack_link_codes_user ON slack_link_codes (user_id);

CREATE TABLE IF NOT EXISTS slack_bot_threads (
  team_id        TEXT NOT NULL,
  channel_id     TEXT NOT NULL,
  thread_ts      TEXT NOT NULL,
  agent_id       UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  owner_user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (team_id, channel_id, thread_ts)
);

CREATE INDEX IF NOT EXISTS slack_bot_threads_created_at ON slack_bot_threads (created_at);
