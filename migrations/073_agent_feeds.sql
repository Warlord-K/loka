CREATE TABLE IF NOT EXISTS agent_feeds (
  id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id          UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  owner_user_id     UUID REFERENCES users(id) ON DELETE CASCADE,
  created_by_agent  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  kind              TEXT NOT NULL CHECK (kind IN ('rss', 'webhook')),
  name              TEXT NOT NULL,
  url               TEXT,
  token             TEXT UNIQUE,
  format            TEXT NOT NULL DEFAULT 'auto' CHECK (format IN ('auto', 'alertmanager', 'json', 'text')),
  filter            TEXT,
  poll_secs         INTEGER NOT NULL DEFAULT 900 CHECK (poll_secs BETWEEN 60 AND 86400),
  next_poll_at      TIMESTAMPTZ,
  last_polled_at    TIMESTAMPTZ,
  last_item_at      TIMESTAMPTZ,
  last_error        TEXT,
  etag              TEXT,
  last_modified     TEXT,
  seen              TEXT[] NOT NULL DEFAULT '{}',
  primed            BOOLEAN NOT NULL DEFAULT FALSE,
  item_count        BIGINT NOT NULL DEFAULT 0,
  paused            BOOLEAN NOT NULL DEFAULT FALSE,
  created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  CHECK ((kind = 'rss' AND url IS NOT NULL) OR (kind = 'webhook' AND token IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS idx_agent_feeds_agent ON agent_feeds (agent_id);
CREATE INDEX IF NOT EXISTS idx_agent_feeds_due ON agent_feeds (next_poll_at) WHERE kind = 'rss' AND NOT paused;

CREATE OR REPLACE FUNCTION world_notify_feed() RETURNS trigger AS $$
BEGIN
  IF NEW.kind = 'rss' THEN
    PERFORM pg_notify('schedules_changed', NEW.id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_feeds_notify ON agent_feeds;
CREATE TRIGGER agent_feeds_notify AFTER INSERT OR UPDATE OF paused, poll_secs, url ON agent_feeds
  FOR EACH ROW EXECUTE FUNCTION world_notify_feed();
