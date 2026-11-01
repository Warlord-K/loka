CREATE OR REPLACE FUNCTION world_track_agent_usage() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'INSERT' THEN
    PERFORM world_add_agent_usage(NEW.agent_id, NEW.cost_usd::numeric, NEW.input_tokens + NEW.cached_input_tokens + NEW.output_tokens);
  ELSIF TG_OP = 'DELETE' THEN
    IF current_setting('loka.cleanup', true) = 'on' THEN
      RETURN NULL;
    END IF;
    PERFORM world_add_agent_usage(OLD.agent_id, -OLD.cost_usd::numeric, -(OLD.input_tokens + OLD.cached_input_tokens + OLD.output_tokens));
  ELSIF NEW.agent_id IS DISTINCT FROM OLD.agent_id THEN
    PERFORM world_add_agent_usage(OLD.agent_id, -OLD.cost_usd::numeric, -(OLD.input_tokens + OLD.cached_input_tokens + OLD.output_tokens));
    PERFORM world_add_agent_usage(NEW.agent_id, NEW.cost_usd::numeric, NEW.input_tokens + NEW.cached_input_tokens + NEW.output_tokens);
  ELSE
    PERFORM world_add_agent_usage(NEW.agent_id, NEW.cost_usd::numeric - OLD.cost_usd::numeric,
      (NEW.input_tokens + NEW.cached_input_tokens + NEW.output_tokens) - (OLD.input_tokens + OLD.cached_input_tokens + OLD.output_tokens));
  END IF;
  RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TABLE IF NOT EXISTS world_cleanups (
    id            BIGSERIAL PRIMARY KEY,
    user_id       UUID REFERENCES users(id) ON DELETE SET NULL,
    org_id        UUID REFERENCES orgs(id) ON DELETE CASCADE,
    agent_id      UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
    by_agent_id   UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
    kind          TEXT NOT NULL,
    older_than    INTEGER NOT NULL,
    rows_removed  BIGINT NOT NULL,
    bytes_freed   BIGINT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_world_cleanups_user ON world_cleanups (user_id, created_at DESC);
