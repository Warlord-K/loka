CREATE TABLE IF NOT EXISTS world_runners (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name           TEXT NOT NULL,
  platform       TEXT NOT NULL DEFAULT 'macos',
  token_hash     BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
  status         TEXT NOT NULL DEFAULT 'offline'
    CHECK (status IN ('online', 'offline', 'paused', 'revoked')),
  capabilities   JSONB NOT NULL DEFAULT '{}'::jsonb,
  last_seen_at   TIMESTAMPTZ,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  revoked_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_world_runners_owner ON world_runners (owner_user_id, created_at);

ALTER TABLE agent_profiles
  ADD COLUMN IF NOT EXISTS runner_id UUID REFERENCES world_runners(id) ON DELETE SET NULL;

ALTER TABLE agent_sessions
  ADD COLUMN IF NOT EXISTS runner_id UUID REFERENCES world_runners(id) ON DELETE RESTRICT;

ALTER TABLE session_runs
  ADD COLUMN IF NOT EXISTS runner_id UUID REFERENCES world_runners(id) ON DELETE RESTRICT,
  ADD COLUMN IF NOT EXISTS lease_id UUID,
  ADD COLUMN IF NOT EXISTS lease_expires_at TIMESTAMPTZ,
  ADD COLUMN IF NOT EXISTS runner_rules_hash TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_profiles_runner ON agent_profiles (runner_id) WHERE runner_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_agent_sessions_runner ON agent_sessions (runner_id) WHERE runner_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_session_runs_runner_queued
  ON session_runs (runner_id, created_at) WHERE status = 'queued' AND runner_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS runner_leases (
  id              UUID PRIMARY KEY,
  run_id          UUID NOT NULL UNIQUE REFERENCES session_runs(id) ON DELETE CASCADE,
  runner_id       UUID NOT NULL REFERENCES world_runners(id) ON DELETE RESTRICT,
  mcp_token_hash  BYTEA NOT NULL UNIQUE CHECK (octet_length(mcp_token_hash) = 32),
  status          TEXT NOT NULL DEFAULT 'active'
    CHECK (status IN ('active', 'finishing', 'completed', 'failed', 'expired', 'cancelled')),
  completion_hash BYTEA,
  completion_payload JSONB,
  completion_processing_at TIMESTAMPTZ,
  expires_at      TIMESTAMPTZ NOT NULL,
  heartbeat_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  finished_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_runner_leases_active
  ON runner_leases (runner_id, expires_at) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS runner_run_inbox (
  run_id    UUID NOT NULL REFERENCES session_runs(id) ON DELETE CASCADE,
  inbox_id  BIGINT NOT NULL REFERENCES agent_inbox(id) ON DELETE CASCADE,
  PRIMARY KEY (run_id, inbox_id)
);

CREATE OR REPLACE FUNCTION world_session_runner_affinity() RETURNS trigger AS $$
DECLARE
  parent_runner UUID;
  agent_runner UUID;
BEGIN
  IF NEW.parent_session_id IS NOT NULL THEN
    SELECT runner_id INTO parent_runner FROM agent_sessions WHERE id = NEW.parent_session_id FOR SHARE;
  END IF;
  SELECT runner_id INTO agent_runner FROM agent_profiles WHERE id = NEW.agent_id FOR SHARE;
  IF parent_runner IS NOT NULL AND agent_runner IS DISTINCT FROM parent_runner THEN
    RAISE EXCEPTION 'agent runner differs from parent session runner';
  END IF;
  NEW.runner_id := COALESCE(parent_runner, agent_runner);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_sessions_runner_affinity ON agent_sessions;
CREATE TRIGGER agent_sessions_runner_affinity BEFORE INSERT OR UPDATE OF agent_id, parent_session_id ON agent_sessions
  FOR EACH ROW EXECUTE FUNCTION world_session_runner_affinity();

CREATE OR REPLACE FUNCTION world_run_runner_affinity() RETURNS trigger AS $$
DECLARE
  session_runner UUID;
BEGIN
  SELECT runner_id INTO session_runner FROM agent_sessions WHERE id = NEW.session_id FOR SHARE;
  NEW.runner_id := session_runner;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_runner_affinity ON session_runs;
CREATE TRIGGER session_runs_runner_affinity BEFORE INSERT OR UPDATE OF session_id ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_run_runner_affinity();

ALTER TABLE session_runs DROP CONSTRAINT IF EXISTS session_runs_kind_check;
ALTER TABLE session_runs ADD CONSTRAINT session_runs_kind_check
  CHECK (kind IN ('turn', 'task', 'review', 'aside', 'followup', 'publish'));

CREATE OR REPLACE FUNCTION world_notify_runner() RETURNS trigger AS $$
BEGIN
  IF NEW.runner_id IS NOT NULL AND NEW.status = 'queued' THEN
    PERFORM pg_notify('runner_jobs', NEW.runner_id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_notify_runner ON session_runs;
CREATE TRIGGER session_runs_notify_runner AFTER INSERT OR UPDATE OF status ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_notify_runner();

CREATE OR REPLACE FUNCTION world_notify_runner_state() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('schedules_changed', NEW.id::text);
  IF NEW.status IN ('online', 'offline') THEN
    PERFORM pg_notify('runner_jobs', NEW.id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_runners_notify_state ON world_runners;
CREATE TRIGGER world_runners_notify_state AFTER UPDATE OF status ON world_runners
  FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status)
  EXECUTE FUNCTION world_notify_runner_state();

CREATE OR REPLACE FUNCTION world_notify_runner_assignment() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('schedules_changed', NEW.id::text);
  IF NEW.runner_id IS NOT NULL THEN
    PERFORM pg_notify('runner_jobs', NEW.runner_id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_profiles_notify_runner_assignment ON agent_profiles;
CREATE TRIGGER agent_profiles_notify_runner_assignment AFTER UPDATE OF runner_id ON agent_profiles
  FOR EACH ROW WHEN (OLD.runner_id IS DISTINCT FROM NEW.runner_id)
  EXECUTE FUNCTION world_notify_runner_assignment();
