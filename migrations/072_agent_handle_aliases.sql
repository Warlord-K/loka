CREATE TABLE agent_slug_aliases (
  slug TEXT PRIMARY KEY,
  agent_id UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX agent_slug_aliases_agent_idx ON agent_slug_aliases(agent_id);
