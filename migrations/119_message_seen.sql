ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS seen_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS channel_message_seen (
    message_id BIGINT NOT NULL REFERENCES channel_messages(id) ON DELETE CASCADE,
    agent_id UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (message_id, agent_id)
);

CREATE INDEX IF NOT EXISTS idx_channel_message_seen_agent ON channel_message_seen (agent_id);
