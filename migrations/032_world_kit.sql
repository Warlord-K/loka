CREATE TABLE IF NOT EXISTS world_mcp_servers (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name        TEXT NOT NULL UNIQUE,
  definition  JSONB NOT NULL,
  scope       TEXT NOT NULL DEFAULT 'agent' CHECK (scope IN ('global', 'agent')),
  enabled     BOOLEAN NOT NULL DEFAULT TRUE,
  created_by  UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS world_skills (
  id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name         TEXT NOT NULL UNIQUE,
  description  TEXT NOT NULL DEFAULT '',
  body         TEXT NOT NULL,
  source_url   TEXT,
  scope        TEXT NOT NULL DEFAULT 'agent' CHECK (scope IN ('global', 'agent')),
  enabled      BOOLEAN NOT NULL DEFAULT TRUE,
  created_by   UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS agent_mcp_servers (
  agent_id   UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  server_id  UUID NOT NULL REFERENCES world_mcp_servers(id) ON DELETE CASCADE,
  PRIMARY KEY (agent_id, server_id)
);

CREATE TABLE IF NOT EXISTS agent_skills (
  agent_id  UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  skill_id  UUID NOT NULL REFERENCES world_skills(id) ON DELETE CASCADE,
  PRIMARY KEY (agent_id, skill_id)
);
