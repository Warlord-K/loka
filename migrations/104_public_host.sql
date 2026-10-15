ALTER TABLE users ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ;
ALTER TABLE users ALTER COLUMN created_at SET DEFAULT NOW();
ALTER TABLE users ADD COLUMN IF NOT EXISTS disabled_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS disabled_reason TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS verify_required BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS token_version INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS world_rate_events (
  id          BIGSERIAL PRIMARY KEY,
  action      TEXT NOT NULL,
  subject     TEXT NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS world_rate_events_lookup_idx ON world_rate_events (action, subject, created_at DESC);
CREATE INDEX IF NOT EXISTS world_rate_events_age_idx ON world_rate_events (created_at);

CREATE OR REPLACE FUNCTION world_rate_take(p_action TEXT, p_subject TEXT, p_max INTEGER, p_window_secs INTEGER, p_day_max INTEGER DEFAULT NULL)
RETURNS BOOLEAN AS $$
DECLARE
  hashed TEXT;
  used INTEGER;
BEGIN
  IF p_subject IS NULL OR btrim(p_subject) = '' THEN
    RETURN TRUE;
  END IF;
  hashed := md5(lower(btrim(p_subject)));
  PERFORM pg_advisory_xact_lock(hashtext('world_rate'), hashtext(p_action || ':' || hashed));
  IF p_max IS NOT NULL AND p_max > 0 THEN
    SELECT COUNT(*) INTO used FROM world_rate_events
    WHERE action = p_action AND subject = hashed AND created_at > NOW() - make_interval(secs => p_window_secs);
    IF used >= p_max THEN
      RETURN FALSE;
    END IF;
  END IF;
  IF p_day_max IS NOT NULL AND p_day_max > 0 THEN
    SELECT COUNT(*) INTO used FROM world_rate_events
    WHERE action = p_action AND subject = hashed AND created_at > NOW() - INTERVAL '1 day';
    IF used >= p_day_max THEN
      RETURN FALSE;
    END IF;
  END IF;
  INSERT INTO world_rate_events (action, subject) VALUES (p_action, hashed);
  RETURN TRUE;
END;
$$ LANGUAGE plpgsql;

CREATE TABLE IF NOT EXISTS email_verifications (
  token_hash  TEXT PRIMARY KEY,
  user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  email       TEXT NOT NULL,
  expires_at  TIMESTAMPTZ NOT NULL,
  used_at     TIMESTAMPTZ,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS email_verifications_user_idx ON email_verifications (user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS email_outbox (
  id          BIGSERIAL PRIMARY KEY,
  kind        TEXT NOT NULL,
  to_email    TEXT NOT NULL,
  subject     TEXT NOT NULL,
  body_text   TEXT NOT NULL,
  body_html   TEXT,
  org_id      UUID REFERENCES orgs(id) ON DELETE CASCADE,
  status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sending', 'sent', 'failed', 'skipped')),
  attempts    INTEGER NOT NULL DEFAULT 0,
  error       TEXT,
  not_before  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  claimed_at  TIMESTAMPTZ,
  sent_at     TIMESTAMPTZ,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS email_outbox_pending_idx ON email_outbox (not_before) WHERE status = 'pending';
CREATE INDEX IF NOT EXISTS email_outbox_org_idx ON email_outbox (org_id, created_at DESC);

CREATE OR REPLACE FUNCTION world_email_outbox_notify() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('email_outbox', NEW.id::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS email_outbox_notify ON email_outbox;
CREATE TRIGGER email_outbox_notify AFTER INSERT ON email_outbox
FOR EACH ROW EXECUTE FUNCTION world_email_outbox_notify();

CREATE TABLE IF NOT EXISTS instance_audit (
  id             BIGSERIAL PRIMARY KEY,
  actor_user_id  UUID REFERENCES users(id) ON DELETE SET NULL,
  action         TEXT NOT NULL,
  target         TEXT,
  detail         JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS instance_audit_recent_idx ON instance_audit (id DESC);

ALTER TABLE orgs ADD COLUMN IF NOT EXISTS icon TEXT;
ALTER TABLE orgs DROP CONSTRAINT IF EXISTS orgs_icon_check;
ALTER TABLE orgs ADD CONSTRAINT orgs_icon_check CHECK (icon IS NULL OR char_length(icon) BETWEEN 1 AND 16);
ALTER TABLE orgs ADD COLUMN IF NOT EXISTS agent_visibility TEXT NOT NULL DEFAULT 'private';
ALTER TABLE orgs DROP CONSTRAINT IF EXISTS orgs_agent_visibility_check;
ALTER TABLE orgs ADD CONSTRAINT orgs_agent_visibility_check CHECK (agent_visibility IN ('private', 'public'));
ALTER TABLE orgs ADD COLUMN IF NOT EXISTS invite_policy TEXT NOT NULL DEFAULT 'admins';
ALTER TABLE orgs DROP CONSTRAINT IF EXISTS orgs_invite_policy_check;
ALTER TABLE orgs ADD CONSTRAINT orgs_invite_policy_check CHECK (invite_policy IN ('admins', 'members'));
ALTER TABLE orgs ADD COLUMN IF NOT EXISTS daily_budget_usd DOUBLE PRECISION;
ALTER TABLE orgs DROP CONSTRAINT IF EXISTS orgs_daily_budget_check;
ALTER TABLE orgs ADD CONSTRAINT orgs_daily_budget_check CHECK (daily_budget_usd IS NULL OR daily_budget_usd > 0);
ALTER TABLE orgs ADD COLUMN IF NOT EXISTS budget_warned_on DATE;
ALTER TABLE orgs ADD COLUMN IF NOT EXISTS budget_paused_on DATE;

CREATE OR REPLACE FUNCTION world_org_spent_today(org UUID) RETURNS DOUBLE PRECISION AS $$
  SELECT COALESCE(SUM(r.cost_usd), 0)::float8
  FROM session_runs r JOIN agent_profiles a ON a.id = r.agent_id
  WHERE a.org_id = org
    AND r.created_at >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - INTERVAL '1 day'
    AND COALESCE(r.finished_at, r.created_at) >= date_trunc('day', NOW() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_enforce_org_budget() RETURNS trigger AS $$
DECLARE
  org UUID;
  budget DOUBLE PRECISION;
  warned DATE;
  paused_on DATE;
  org_name TEXT;
  spent DOUBLE PRECISION;
  today DATE := (NOW() AT TIME ZONE 'UTC')::date;
  reason TEXT;
  paused INTEGER;
BEGIN
  IF COALESCE(NEW.cost_usd, 0) <= COALESCE(OLD.cost_usd, 0) THEN
    RETURN NEW;
  END IF;
  SELECT a.org_id INTO org FROM agent_profiles a WHERE a.id = NEW.agent_id;
  IF org IS NULL THEN
    RETURN NEW;
  END IF;
  SELECT o.daily_budget_usd, o.budget_warned_on, o.budget_paused_on, o.name INTO budget, warned, paused_on, org_name
  FROM orgs o WHERE o.id = org;
  IF budget IS NULL OR budget <= 0 THEN
    RETURN NEW;
  END IF;
  spent := world_org_spent_today(org);
  IF spent >= budget THEN
    reason := format('Organization daily budget of $%s reached ($%s spent today). Resumes at 00:00 UTC, or an admin raises it.',
      to_char(budget, 'FM999990.00'), to_char(spent, 'FM999990.00'));
    UPDATE agent_profiles SET paused_reason = reason, updated_at = NOW()
    WHERE org_id = org AND paused_reason IS NULL;
    GET DIAGNOSTICS paused = ROW_COUNT;
    IF paused_on IS DISTINCT FROM today THEN
      UPDATE orgs SET budget_paused_on = today, budget_warned_on = today WHERE id = org;
      INSERT INTO world_events (type, org_id, run_id, payload)
      VALUES ('org.budget', org, NEW.id, jsonb_build_object(
        'paused', true, 'text', reason, 'org', org_name, 'org_id', org,
        'budget_usd', budget, 'spent_usd', spent, 'paused_agents', paused,
        'audience', (SELECT COALESCE(jsonb_agg(m.user_id), '[]'::jsonb) FROM org_members m WHERE m.org_id = org AND m.role IN ('owner', 'admin'))));
    END IF;
  ELSIF spent >= budget * 0.8 AND warned IS DISTINCT FROM today THEN
    UPDATE orgs SET budget_warned_on = today WHERE id = org;
    INSERT INTO world_events (type, org_id, run_id, payload)
    VALUES ('org.budget', org, NEW.id, jsonb_build_object(
      'paused', false,
      'text', format('%s has used $%s of its $%s daily budget. Every agent here pauses when it runs out.',
        org_name, to_char(spent, 'FM999990.00'), to_char(budget, 'FM999990.00')),
      'org', org_name, 'org_id', org, 'budget_usd', budget, 'spent_usd', spent,
      'audience', (SELECT COALESCE(jsonb_agg(m.user_id), '[]'::jsonb) FROM org_members m WHERE m.org_id = org AND m.role IN ('owner', 'admin'))));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_org_budget ON session_runs;
CREATE TRIGGER session_runs_org_budget AFTER UPDATE OF cost_usd ON session_runs
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
      AND (o.daily_budget_usd IS NULL
           OR o.budget_paused_on IS NULL
           OR o.budget_paused_on < today
           OR o.daily_budget_usd > world_org_spent_today(o.id))
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
             WHEN o.budget_paused_on = (NOW() AT TIME ZONE 'UTC')::date AND o.daily_budget_usd IS NOT NULL
               THEN format('Organization daily budget of $%s reached. Resumes at 00:00 UTC, or an admin raises it.', to_char(o.daily_budget_usd, 'FM999990.00'))
           END
      INTO org_pause
    FROM orgs o WHERE o.id = NEW.org_id;
    NEW.paused_reason := org_pause;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
