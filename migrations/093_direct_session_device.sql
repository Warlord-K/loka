CREATE OR REPLACE FUNCTION world_session_runner_affinity() RETURNS trigger AS $$
DECLARE
  parent_runner UUID;
  agent_runner UUID;
  agent_owner UUID;
BEGIN
  SELECT runner_id, owner_user_id INTO agent_runner, agent_owner FROM agent_profiles WHERE id = NEW.agent_id FOR SHARE;
  IF NEW.role = 'direct' AND NEW.runner_id IS NOT NULL THEN
    IF NOT EXISTS (
      SELECT 1 FROM world_runners WHERE id = NEW.runner_id AND owner_user_id IS NOT DISTINCT FROM agent_owner
    ) THEN
      RAISE EXCEPTION 'folder sessions run only on a device of the agent owner';
    END IF;
    RETURN NEW;
  END IF;
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
