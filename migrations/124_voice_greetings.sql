CREATE TABLE IF NOT EXISTS agent_voice_greetings (
    agent_id UUID PRIMARY KEY REFERENCES agent_profiles(id) ON DELETE CASCADE,
    voice TEXT NOT NULL,
    phrase TEXT NOT NULL,
    audio BYTEA NOT NULL,
    sample_rate INT NOT NULL DEFAULT 24000,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
