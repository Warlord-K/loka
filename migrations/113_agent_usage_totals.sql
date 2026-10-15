CREATE TABLE IF NOT EXISTS agent_usage_totals (
  agent_id UUID PRIMARY KEY REFERENCES agent_profiles(id) ON DELETE CASCADE,
  cost_usd NUMERIC NOT NULL DEFAULT 0,
  tokens BIGINT NOT NULL DEFAULT 0
);

CREATE OR REPLACE FUNCTION world_add_agent_usage(agent UUID, cost NUMERIC, tokens BIGINT) RETURNS void AS $$
BEGIN
  IF agent IS NULL OR (cost = 0 AND tokens = 0) THEN
    RETURN;
  END IF;
  INSERT INTO agent_usage_totals AS t (agent_id, cost_usd, tokens)
  SELECT agent, cost, tokens WHERE EXISTS (SELECT 1 FROM agent_profiles WHERE id = agent)
  ON CONFLICT (agent_id) DO UPDATE SET cost_usd = t.cost_usd + EXCLUDED.cost_usd, tokens = t.tokens + EXCLUDED.tokens;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_track_agent_usage() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'INSERT' THEN
    PERFORM world_add_agent_usage(NEW.agent_id, NEW.cost_usd::numeric, NEW.input_tokens + NEW.cached_input_tokens + NEW.output_tokens);
  ELSIF TG_OP = 'DELETE' THEN
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

LOCK TABLE session_runs IN SHARE ROW EXCLUSIVE MODE;

DROP TRIGGER IF EXISTS world_agent_usage_totals ON session_runs;
CREATE TRIGGER world_agent_usage_totals
  AFTER INSERT OR DELETE OR UPDATE OF agent_id, cost_usd, input_tokens, cached_input_tokens, output_tokens ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_track_agent_usage();

INSERT INTO agent_usage_totals (agent_id, cost_usd, tokens)
SELECT r.agent_id, SUM(r.cost_usd::numeric), SUM(r.input_tokens + r.cached_input_tokens + r.output_tokens)
FROM session_runs r JOIN agent_profiles a ON a.id = r.agent_id
GROUP BY r.agent_id
ON CONFLICT (agent_id) DO UPDATE SET cost_usd = EXCLUDED.cost_usd, tokens = EXCLUDED.tokens;
