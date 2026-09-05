CREATE OR REPLACE FUNCTION world_session_runner_affinity() RETURNS trigger AS $$
DECLARE
  parent_runner UUID;
  agent_runner UUID;
BEGIN
  SELECT runner_id INTO agent_runner FROM agent_profiles WHERE id = NEW.agent_id FOR SHARE;
  IF NEW.parent_session_id IS NOT NULL THEN
    SELECT runner_id INTO parent_runner FROM agent_sessions WHERE id = NEW.parent_session_id FOR SHARE;
  END IF;
  IF parent_runner IS NOT NULL AND agent_runner IS DISTINCT FROM parent_runner THEN
    RAISE EXCEPTION 'agent runner differs from parent session runner';
  END IF;
  NEW.runner_id := COALESCE(parent_runner, agent_runner);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
