CREATE OR REPLACE FUNCTION world_inbox_retry_delay() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'pending' AND OLD.status = 'processing' AND NEW.retry_at IS NOT DISTINCT FROM OLD.retry_at THEN
    NEW.retry_at := NOW() + make_interval(secs => 60 * GREATEST(NEW.attempts, 1));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

ALTER TABLE agent_profiles
  ADD COLUMN IF NOT EXISTS daily_budget_usd DOUBLE PRECISION,
  ADD COLUMN IF NOT EXISTS budget_warned_on DATE,
  ADD COLUMN IF NOT EXISTS budget_paused_on DATE;

CREATE OR REPLACE FUNCTION world_agent_spent_today(agent UUID) RETURNS DOUBLE PRECISION AS $$
  SELECT COALESCE(SUM(cost_usd), 0)::float8 FROM session_runs
  WHERE agent_id = agent
    AND COALESCE(finished_at, created_at) >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_enforce_daily_budget() RETURNS trigger AS $$
DECLARE
  budget DOUBLE PRECISION;
  warned DATE;
  owner UUID;
  agent_name TEXT;
  spent DOUBLE PRECISION;
  today DATE := (NOW() AT TIME ZONE 'UTC')::date;
  reason TEXT;
BEGIN
  IF COALESCE(NEW.cost_usd, 0) <= COALESCE(OLD.cost_usd, 0) THEN
    RETURN NEW;
  END IF;
  SELECT daily_budget_usd, budget_warned_on, owner_user_id, name INTO budget, warned, owner, agent_name
  FROM agent_profiles WHERE id = NEW.agent_id;
  IF budget IS NULL OR budget <= 0 THEN
    RETURN NEW;
  END IF;
  spent := world_agent_spent_today(NEW.agent_id);
  IF spent >= budget THEN
    reason := format('Daily budget of $%s reached ($%s spent today). Resumes at 00:00 UTC, or raise the budget.',
      to_char(budget, 'FM999990.00'), to_char(spent, 'FM999990.00'));
    UPDATE agent_profiles SET paused_reason = reason, budget_paused_on = today, budget_warned_on = today, updated_at = NOW()
    WHERE id = NEW.agent_id AND paused_reason IS NULL;
    IF FOUND THEN
      INSERT INTO world_events (type, agent_id, run_id, payload)
      VALUES ('agent.budget', NEW.agent_id, NEW.id, jsonb_build_object(
        'paused', true, 'text', reason, 'agent', agent_name, 'owner_user_id', owner,
        'budget_usd', budget, 'spent_usd', spent));
    END IF;
  ELSIF spent >= budget * 0.8 AND warned IS DISTINCT FROM today THEN
    UPDATE agent_profiles SET budget_warned_on = today WHERE id = NEW.agent_id;
    INSERT INTO world_events (type, agent_id, run_id, payload)
    VALUES ('agent.budget', NEW.agent_id, NEW.id, jsonb_build_object(
      'paused', false,
      'text', format('$%s of its $%s daily budget is used. It pauses when the budget runs out.',
        to_char(spent, 'FM999990.00'), to_char(budget, 'FM999990.00')),
      'agent', agent_name, 'owner_user_id', owner, 'budget_usd', budget, 'spent_usd', spent));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_daily_budget ON session_runs;
CREATE TRIGGER session_runs_daily_budget AFTER UPDATE OF cost_usd ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_enforce_daily_budget();
