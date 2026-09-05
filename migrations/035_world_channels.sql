CREATE TABLE IF NOT EXISTS channels (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name        TEXT NOT NULL,
  topic       TEXT,
  kind        TEXT NOT NULL DEFAULT 'channel' CHECK (kind IN ('channel', 'dm')),
  created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_channels_name ON channels (lower(name)) WHERE kind = 'channel';

CREATE TABLE IF NOT EXISTS channel_members (
  channel_id   UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
  member_kind  TEXT NOT NULL CHECK (member_kind IN ('user', 'agent')),
  member_id    UUID NOT NULL,
  joined_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (channel_id, member_kind, member_id)
);

CREATE INDEX IF NOT EXISTS idx_channel_members_member ON channel_members (member_kind, member_id);

CREATE TABLE IF NOT EXISTS channel_messages (
  id               BIGSERIAL PRIMARY KEY,
  channel_id       UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
  author_kind      TEXT NOT NULL CHECK (author_kind IN ('user', 'agent', 'system')),
  author_user_id   UUID REFERENCES users(id) ON DELETE SET NULL,
  author_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  body             TEXT NOT NULL,
  mentions         JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_channel_messages_channel ON channel_messages (channel_id, id);

CREATE TABLE IF NOT EXISTS channel_reads (
  user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  channel_id    UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
  last_read_id  BIGINT NOT NULL DEFAULT 0,
  PRIMARY KEY (user_id, channel_id)
);

INSERT INTO channels (name, topic)
SELECT 'general', 'Everyone, people and agents'
WHERE NOT EXISTS (SELECT 1 FROM channels WHERE kind = 'channel' AND lower(name) = 'general');
