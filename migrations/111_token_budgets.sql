ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS daily_token_budget BIGINT;
ALTER TABLE agent_profiles DROP CONSTRAINT IF EXISTS agent_profiles_daily_token_budget_check;
ALTER TABLE agent_profiles ADD CONSTRAINT agent_profiles_daily_token_budget_check CHECK (daily_token_budget IS NULL OR daily_token_budget > 0);
ALTER TABLE orgs ADD COLUMN IF NOT EXISTS daily_token_budget BIGINT;
ALTER TABLE orgs DROP CONSTRAINT IF EXISTS orgs_daily_token_budget_check;
ALTER TABLE orgs ADD CONSTRAINT orgs_daily_token_budget_check CHECK (daily_token_budget IS NULL OR daily_token_budget > 0);

CREATE OR REPLACE FUNCTION world_tokens_text(n BIGINT) RETURNS TEXT AS $$
  SELECT CASE
           WHEN n >= 1000000 THEN rtrim(rtrim(to_char(n / 1000000.0, 'FM999999990.0'), '0'), '.') || 'M'
           WHEN n >= 1000 THEN rtrim(rtrim(to_char(n / 1000.0, 'FM999990.0'), '0'), '.') || 'K'
           ELSE n::text
         END || ' tokens'
$$ LANGUAGE sql IMMUTABLE;

CREATE OR REPLACE FUNCTION world_agent_tokens_today(agent UUID) RETURNS BIGINT AS $$
  SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::int8 FROM session_runs
  WHERE agent_id = agent
    AND COALESCE(finished_at, created_at) >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_org_tokens_today(org UUID) RETURNS BIGINT AS $$
  SELECT COALESCE(SUM(r.input_tokens + r.output_tokens), 0)::int8
  FROM session_runs r JOIN agent_profiles a ON a.id = r.agent_id
  WHERE a.org_id = org
    AND r.created_at >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - INTERVAL '1 day'
    AND COALESCE(r.finished_at, r.created_at) >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_run_spend_grew(old_run session_runs, new_run session_runs) RETURNS BOOLEAN AS $$
  SELECT COALESCE(new_run.cost_usd, 0) > COALESCE(old_run.cost_usd, 0)
      OR COALESCE(new_run.input_tokens, 0) + COALESCE(new_run.output_tokens, 0)
         > COALESCE(old_run.input_tokens, 0) + COALESCE(old_run.output_tokens, 0)
$$ LANGUAGE sql IMMUTABLE;

CREATE OR REPLACE FUNCTION world_agent_over_budget(agent UUID) RETURNS TEXT AS $$
  SELECT CASE
           WHEN a.daily_budget_usd > 0 AND world_agent_spent_today(a.id) >= a.daily_budget_usd
             THEN format('Daily budget of $%s reached ($%s spent today). Resumes at 00:00 UTC, or raise the budget.',
                         to_char(a.daily_budget_usd, 'FM999990.00'), to_char(world_agent_spent_today(a.id), 'FM999990.00'))
           WHEN a.daily_token_budget > 0 AND world_agent_tokens_today(a.id) >= a.daily_token_budget
             THEN format('Daily budget of %s reached (%s used today). Resumes at 00:00 UTC, or raise the budget.',
                         world_tokens_text(a.daily_token_budget), world_tokens_text(world_agent_tokens_today(a.id)))
         END
  FROM agent_profiles a WHERE a.id = agent
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_org_over_budget(org UUID) RETURNS TEXT AS $$
  SELECT CASE
           WHEN o.daily_budget_usd > 0 AND world_org_spent_today(o.id) >= o.daily_budget_usd
             THEN format('Organization daily budget of $%s reached ($%s spent today). Resumes at 00:00 UTC, or an admin raises it.',
                         to_char(o.daily_budget_usd, 'FM999990.00'), to_char(world_org_spent_today(o.id), 'FM999990.00'))
           WHEN o.daily_token_budget > 0 AND world_org_tokens_today(o.id) >= o.daily_token_budget
             THEN format('Organization daily budget of %s reached (%s used today). Resumes at 00:00 UTC, or an admin raises it.',
                         world_tokens_text(o.daily_token_budget), world_tokens_text(world_org_tokens_today(o.id)))
         END
  FROM orgs o WHERE o.id = org
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_enforce_daily_budget() RETURNS trigger AS $$
DECLARE
  budget DOUBLE PRECISION;
  token_budget BIGINT;
  warned DATE;
  owner UUID;
  agent_name TEXT;
  spent DOUBLE PRECISION := 0;
  used BIGINT := 0;
  today DATE := (NOW() AT TIME ZONE 'UTC')::date;
  reason TEXT;
  warning TEXT;
BEGIN
  IF NOT world_run_spend_grew(OLD, NEW) THEN
    RETURN NEW;
  END IF;
  SELECT daily_budget_usd, daily_token_budget, budget_warned_on, owner_user_id, name
    INTO budget, token_budget, warned, owner, agent_name
  FROM agent_profiles WHERE id = NEW.agent_id;
  IF COALESCE(budget, 0) <= 0 AND COALESCE(token_budget, 0) <= 0 THEN
    RETURN NEW;
  END IF;
  IF budget > 0 THEN
    spent := world_agent_spent_today(NEW.agent_id);
  END IF;
  IF token_budget > 0 THEN
    used := world_agent_tokens_today(NEW.agent_id);
  END IF;
  reason := world_agent_over_budget(NEW.agent_id);
  IF reason IS NOT NULL THEN
    UPDATE agent_profiles SET paused_reason = reason, budget_paused_on = today, budget_warned_on = today, updated_at = NOW()
    WHERE id = NEW.agent_id AND paused_reason IS NULL;
    IF FOUND THEN
      INSERT INTO world_events (type, agent_id, run_id, payload)
      VALUES ('agent.budget', NEW.agent_id, NEW.id, jsonb_build_object(
        'paused', true, 'text', reason, 'agent', agent_name, 'owner_user_id', owner,
        'budget_usd', budget, 'spent_usd', spent, 'token_budget', token_budget, 'tokens_used', used));
    END IF;
  ELSIF warned IS DISTINCT FROM today AND ((budget > 0 AND spent >= budget * 0.8) OR (token_budget > 0 AND used >= token_budget * 0.8)) THEN
    warning := CASE
      WHEN budget > 0 AND spent >= budget * 0.8
        THEN format('$%s of its $%s daily budget is used. It pauses when the budget runs out.',
                    to_char(spent, 'FM999990.00'), to_char(budget, 'FM999990.00'))
      ELSE format('%s of its %s daily budget is used. It pauses when the budget runs out.',
                  world_tokens_text(used), world_tokens_text(token_budget))
    END;
    UPDATE agent_profiles SET budget_warned_on = today WHERE id = NEW.agent_id;
    INSERT INTO world_events (type, agent_id, run_id, payload)
    VALUES ('agent.budget', NEW.agent_id, NEW.id, jsonb_build_object(
      'paused', false, 'text', warning, 'agent', agent_name, 'owner_user_id', owner,
      'budget_usd', budget, 'spent_usd', spent, 'token_budget', token_budget, 'tokens_used', used));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_daily_budget ON session_runs;
CREATE TRIGGER session_runs_daily_budget AFTER UPDATE OF cost_usd, input_tokens, output_tokens ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_enforce_daily_budget();

CREATE OR REPLACE FUNCTION world_enforce_org_budget() RETURNS trigger AS $$
DECLARE
  org UUID;
  budget DOUBLE PRECISION;
  token_budget BIGINT;
  warned DATE;
  paused_on DATE;
  org_name TEXT;
  spent DOUBLE PRECISION := 0;
  used BIGINT := 0;
  today DATE := (NOW() AT TIME ZONE 'UTC')::date;
  reason TEXT;
  warning TEXT;
  paused INTEGER;
  admins JSONB;
BEGIN
  IF NOT world_run_spend_grew(OLD, NEW) THEN
    RETURN NEW;
  END IF;
  SELECT a.org_id INTO org FROM agent_profiles a WHERE a.id = NEW.agent_id;
  IF org IS NULL THEN
    RETURN NEW;
  END IF;
  SELECT o.daily_budget_usd, o.daily_token_budget, o.budget_warned_on, o.budget_paused_on, o.name
    INTO budget, token_budget, warned, paused_on, org_name
  FROM orgs o WHERE o.id = org;
  IF COALESCE(budget, 0) <= 0 AND COALESCE(token_budget, 0) <= 0 THEN
    RETURN NEW;
  END IF;
  IF budget > 0 THEN
    spent := world_org_spent_today(org);
  END IF;
  IF token_budget > 0 THEN
    used := world_org_tokens_today(org);
  END IF;
  SELECT COALESCE(jsonb_agg(m.user_id), '[]'::jsonb) INTO admins
  FROM org_members m WHERE m.org_id = org AND m.role IN ('owner', 'admin');
  reason := world_org_over_budget(org);
  IF reason IS NOT NULL THEN
    UPDATE agent_profiles SET paused_reason = reason, updated_at = NOW()
    WHERE org_id = org AND paused_reason IS NULL;
    GET DIAGNOSTICS paused = ROW_COUNT;
    IF paused_on IS DISTINCT FROM today THEN
      UPDATE orgs SET budget_paused_on = today, budget_warned_on = today WHERE id = org;
      INSERT INTO world_events (type, org_id, run_id, payload)
      VALUES ('org.budget', org, NEW.id, jsonb_build_object(
        'paused', true, 'text', reason, 'org', org_name, 'org_id', org,
        'budget_usd', budget, 'spent_usd', spent, 'token_budget', token_budget, 'tokens_used', used,
        'paused_agents', paused, 'audience', admins));
    END IF;
  ELSIF warned IS DISTINCT FROM today AND ((budget > 0 AND spent >= budget * 0.8) OR (token_budget > 0 AND used >= token_budget * 0.8)) THEN
    warning := CASE
      WHEN budget > 0 AND spent >= budget * 0.8
        THEN format('%s has used $%s of its $%s daily budget. Every agent here pauses when it runs out.',
                    org_name, to_char(spent, 'FM999990.00'), to_char(budget, 'FM999990.00'))
      ELSE format('%s has used %s of its %s daily budget. Every agent here pauses when it runs out.',
                  org_name, world_tokens_text(used), world_tokens_text(token_budget))
    END;
    UPDATE orgs SET budget_warned_on = today WHERE id = org;
    INSERT INTO world_events (type, org_id, run_id, payload)
    VALUES ('org.budget', org, NEW.id, jsonb_build_object(
      'paused', false, 'text', warning, 'org', org_name, 'org_id', org,
      'budget_usd', budget, 'spent_usd', spent, 'token_budget', token_budget, 'tokens_used', used,
      'audience', admins));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_org_budget ON session_runs;
CREATE TRIGGER session_runs_org_budget AFTER UPDATE OF cost_usd, input_tokens, output_tokens ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_enforce_org_budget();

CREATE OR REPLACE FUNCTION world_release_org_budgets(p_org UUID) RETURNS SETOF UUID AS $$
DECLARE
  today DATE := (NOW() AT TIME ZONE 'UTC')::date;
BEGIN
  RETURN QUERY
  WITH open_orgs AS (
    SELECT o.id FROM orgs o
    WHERE (p_org IS NULL OR o.id = p_org)
      AND EXISTS (SELECT 1 FROM agent_profiles a WHERE a.org_id = o.id
                  AND starts_with(COALESCE(a.paused_reason, ''), 'Organization daily budget of'))
      AND (o.budget_paused_on IS NULL
           OR o.budget_paused_on < today
           OR world_org_over_budget(o.id) IS NULL)
  ), cleared AS (
    UPDATE orgs SET budget_paused_on = NULL WHERE id IN (SELECT id FROM open_orgs) RETURNING id
  )
  UPDATE agent_profiles a SET paused_reason = NULL, updated_at = NOW()
  WHERE a.org_id IN (SELECT id FROM cleared)
    AND starts_with(COALESCE(a.paused_reason, ''), 'Organization daily budget of')
  RETURNING a.id;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_fill_agent_org() RETURNS trigger AS $$
DECLARE
  org_pause TEXT;
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.owner_user_id), world_only_org(), world_default_org());
  IF NEW.paused_reason IS NULL THEN
    SELECT CASE
             WHEN o.suspended_at IS NOT NULL THEN 'Organization suspended'
             WHEN o.budget_paused_on = (NOW() AT TIME ZONE 'UTC')::date THEN world_org_over_budget(o.id)
           END
      INTO org_pause
    FROM orgs o WHERE o.id = NEW.org_id;
    NEW.paused_reason := org_pause;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
