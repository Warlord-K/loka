ALTER TABLE channel_messages ADD COLUMN IF NOT EXISTS reply_to BIGINT REFERENCES channel_messages(id) ON DELETE CASCADE;
ALTER TABLE channel_messages ADD COLUMN IF NOT EXISTS reply_count INT NOT NULL DEFAULT 0;
ALTER TABLE channel_messages ADD COLUMN IF NOT EXISTS last_reply_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_channel_messages_replies ON channel_messages (reply_to, id) WHERE reply_to IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_channel_messages_roots ON channel_messages (channel_id, id) WHERE reply_to IS NULL;

ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS reply_to UUID REFERENCES agent_messages(id) ON DELETE CASCADE;
ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS reply_count INT NOT NULL DEFAULT 0;
ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS last_reply_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_agent_messages_replies ON agent_messages (reply_to, created_at) WHERE reply_to IS NOT NULL;

CREATE OR REPLACE FUNCTION world_channel_reply_added() RETURNS trigger AS $$
BEGIN
  UPDATE channel_messages
     SET reply_count = reply_count + 1,
         last_reply_at = GREATEST(COALESCE(last_reply_at, NEW.created_at), NEW.created_at)
   WHERE id = NEW.reply_to;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_channel_reply_removed() RETURNS trigger AS $$
BEGIN
  UPDATE channel_messages
     SET reply_count = GREATEST(reply_count - 1, 0),
         last_reply_at = (SELECT MAX(r.created_at) FROM channel_messages r WHERE r.reply_to = OLD.reply_to AND r.id <> OLD.id)
   WHERE id = OLD.reply_to;
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_channel_reply_added ON channel_messages;
CREATE TRIGGER world_channel_reply_added AFTER INSERT ON channel_messages
  FOR EACH ROW WHEN (NEW.reply_to IS NOT NULL) EXECUTE FUNCTION world_channel_reply_added();

DROP TRIGGER IF EXISTS world_channel_reply_removed ON channel_messages;
CREATE TRIGGER world_channel_reply_removed AFTER DELETE ON channel_messages
  FOR EACH ROW WHEN (OLD.reply_to IS NOT NULL) EXECUTE FUNCTION world_channel_reply_removed();

CREATE OR REPLACE FUNCTION world_agent_reply_added() RETURNS trigger AS $$
BEGIN
  UPDATE agent_messages
     SET reply_count = reply_count + 1,
         last_reply_at = GREATEST(COALESCE(last_reply_at, NEW.created_at), NEW.created_at)
   WHERE id = NEW.reply_to;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_agent_reply_removed() RETURNS trigger AS $$
BEGIN
  UPDATE agent_messages
     SET reply_count = GREATEST(reply_count - 1, 0),
         last_reply_at = (SELECT MAX(r.created_at) FROM agent_messages r WHERE r.reply_to = OLD.reply_to AND r.id <> OLD.id)
   WHERE id = OLD.reply_to;
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_agent_reply_added ON agent_messages;
CREATE TRIGGER world_agent_reply_added AFTER INSERT ON agent_messages
  FOR EACH ROW WHEN (NEW.reply_to IS NOT NULL) EXECUTE FUNCTION world_agent_reply_added();

DROP TRIGGER IF EXISTS world_agent_reply_removed ON agent_messages;
CREATE TRIGGER world_agent_reply_removed AFTER DELETE ON agent_messages
  FOR EACH ROW WHEN (OLD.reply_to IS NOT NULL) EXECUTE FUNCTION world_agent_reply_removed();
