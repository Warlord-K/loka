CREATE TABLE IF NOT EXISTS starter_teams (
  owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  org_key       UUID NOT NULL,
  rule_id       UUID REFERENCES world_rules(id) ON DELETE SET NULL,
  team_id       UUID REFERENCES agent_teams(id) ON DELETE SET NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (owner_user_id, org_key)
);

CREATE TABLE IF NOT EXISTS starter_team_seats (
  owner_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  org_key       UUID NOT NULL,
  seat          TEXT NOT NULL,
  agent_id      UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (owner_user_id, org_key, seat)
);

CREATE INDEX IF NOT EXISTS idx_starter_team_seats_agent ON starter_team_seats (agent_id);
