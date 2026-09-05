CREATE TABLE IF NOT EXISTS agent_omp_configs (
    agent_id UUID PRIMARY KEY REFERENCES agent_profiles(id) ON DELETE CASCADE,
    config TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
