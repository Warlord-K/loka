ALTER TABLE agent_profiles
  ADD COLUMN IF NOT EXISTS owner_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
  ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'public'
    CHECK (visibility IN ('private', 'public')),
  ADD COLUMN IF NOT EXISTS role TEXT NOT NULL DEFAULT 'worker'
    CHECK (role IN ('worker', 'assistant')),
  ADD COLUMN IF NOT EXISTS persona TEXT,
  ADD COLUMN IF NOT EXISTS look JSONB NOT NULL DEFAULT '{}'::jsonb,
  ADD COLUMN IF NOT EXISTS gear JSONB NOT NULL DEFAULT '{}'::jsonb,
  ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'idle'
    CHECK (status IN ('idle', 'thinking', 'away', 'waiting', 'error')),
  ADD COLUMN IF NOT EXISTS status_detail TEXT,
  ADD COLUMN IF NOT EXISTS inbox_claimed_by TEXT,
  ADD COLUMN IF NOT EXISTS inbox_claimed_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS agent_sessions (
  id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id           UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  role               TEXT NOT NULL CHECK (role IN ('main', 'task', 'aside', 'review', 'designer')),
  backend            TEXT NOT NULL,
  model              TEXT NOT NULL,
  cli_session_id     TEXT,
  parent_session_id  UUID REFERENCES agent_sessions(id) ON DELETE SET NULL,
  task_id            UUID REFERENCES agent_tasks(id) ON DELETE SET NULL,
  working_dir        TEXT,
  status             TEXT NOT NULL DEFAULT 'active'
    CHECK (status IN ('active', 'completed', 'failed', 'archived')),
  usage_baseline     JSONB NOT NULL DEFAULT '{}'::jsonb,
  context_tokens     BIGINT NOT NULL DEFAULT 0,
  review_rounds      INTEGER NOT NULL DEFAULT 0,
  summary            TEXT,
  created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_sessions_one_main
  ON agent_sessions (agent_id) WHERE role = 'main' AND status = 'active';
CREATE INDEX IF NOT EXISTS idx_agent_sessions_agent ON agent_sessions (agent_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_task ON agent_sessions (task_id) WHERE task_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS session_runs (
  id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  session_id           UUID NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
  agent_id             UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  task_id              UUID REFERENCES agent_tasks(id) ON DELETE SET NULL,
  kind                 TEXT NOT NULL CHECK (kind IN ('turn', 'task', 'review')),
  status               TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'completed', 'failed', 'timed_out', 'cancelled')),
  fork_from            UUID REFERENCES agent_sessions(id) ON DELETE SET NULL,
  prompt               TEXT NOT NULL,
  output               TEXT,
  error                TEXT,
  timeout_secs         INTEGER NOT NULL DEFAULT 3600,
  cost_usd             DOUBLE PRECISION NOT NULL DEFAULT 0,
  input_tokens         BIGINT NOT NULL DEFAULT 0,
  cached_input_tokens  BIGINT NOT NULL DEFAULT 0,
  output_tokens        BIGINT NOT NULL DEFAULT 0,
  claimed_by           TEXT,
  heartbeat_at         TIMESTAMPTZ,
  created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  started_at           TIMESTAMPTZ,
  finished_at          TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_session_runs_queued ON session_runs (created_at) WHERE status = 'queued';
CREATE INDEX IF NOT EXISTS idx_session_runs_session ON session_runs (session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_session_runs_agent ON session_runs (agent_id, created_at DESC);

CREATE TABLE IF NOT EXISTS world_events (
  seq            BIGSERIAL PRIMARY KEY,
  type           TEXT NOT NULL,
  agent_id       UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  session_id     UUID REFERENCES agent_sessions(id) ON DELETE SET NULL,
  run_id         UUID REFERENCES session_runs(id) ON DELETE SET NULL,
  task_id        UUID REFERENCES agent_tasks(id) ON DELETE SET NULL,
  actor_user_id  UUID REFERENCES users(id) ON DELETE SET NULL,
  payload        JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_world_events_agent ON world_events (agent_id, seq DESC);
CREATE INDEX IF NOT EXISTS idx_world_events_type ON world_events (type, seq DESC);

CREATE TABLE IF NOT EXISTS agent_inbox (
  id          BIGSERIAL PRIMARY KEY,
  agent_id    UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  kind        TEXT NOT NULL,
  event_seq   BIGINT REFERENCES world_events(seq) ON DELETE SET NULL,
  payload     JSONB NOT NULL DEFAULT '{}'::jsonb,
  status      TEXT NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'processing', 'done', 'failed')),
  attempts    INTEGER NOT NULL DEFAULT 0,
  error       TEXT,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  done_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_agent_inbox_pending ON agent_inbox (agent_id, id) WHERE status = 'pending';

CREATE TABLE IF NOT EXISTS agent_messages (
  id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id         UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  session_id       UUID REFERENCES agent_sessions(id) ON DELETE SET NULL,
  run_id           UUID REFERENCES session_runs(id) ON DELETE SET NULL,
  author_kind      TEXT NOT NULL CHECK (author_kind IN ('user', 'agent', 'system')),
  author_user_id   UUID REFERENCES users(id) ON DELETE SET NULL,
  author_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  body             TEXT NOT NULL,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_messages_agent ON agent_messages (agent_id, created_at);

ALTER TABLE schedules
  ADD COLUMN IF NOT EXISTS agent_id UUID REFERENCES agent_profiles(id) ON DELETE CASCADE,
  ADD COLUMN IF NOT EXISTS next_run_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_schedules_agent_next ON schedules (next_run_at)
  WHERE agent_id IS NOT NULL AND enabled = true;

CREATE OR REPLACE FUNCTION world_notify_event() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('world_events', json_build_object(
    'seq', NEW.seq, 'type', NEW.type, 'agent_id', NEW.agent_id, 'task_id', NEW.task_id)::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_events_notify ON world_events;
CREATE TRIGGER world_events_notify AFTER INSERT ON world_events
  FOR EACH ROW EXECUTE FUNCTION world_notify_event();

CREATE OR REPLACE FUNCTION world_notify_inbox() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('agent_inbox', NEW.agent_id::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_inbox_notify ON agent_inbox;
CREATE TRIGGER agent_inbox_notify AFTER INSERT ON agent_inbox
  FOR EACH ROW EXECUTE FUNCTION world_notify_inbox();

CREATE OR REPLACE FUNCTION world_notify_run() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'queued' THEN
    PERFORM pg_notify('session_runs', NEW.id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS session_runs_notify ON session_runs;
CREATE TRIGGER session_runs_notify AFTER INSERT OR UPDATE OF status ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_notify_run();

CREATE OR REPLACE FUNCTION world_notify_schedule() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('schedules_changed', COALESCE(NEW.id, OLD.id)::text);
  RETURN COALESCE(NEW, OLD);
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS schedules_notify ON schedules;
CREATE TRIGGER schedules_notify AFTER INSERT OR UPDATE OR DELETE ON schedules
  FOR EACH ROW EXECUTE FUNCTION world_notify_schedule();

CREATE OR REPLACE FUNCTION world_task_assigned() RETURNS trigger AS $$
DECLARE
  new_seq BIGINT;
  body JSONB;
BEGIN
  IF NEW.assigned_agent_id IS NULL OR NEW.assignee_user_id IS NOT NULL THEN
    RETURN NEW;
  END IF;
  IF TG_OP = 'UPDATE' AND OLD.assigned_agent_id IS NOT DISTINCT FROM NEW.assigned_agent_id THEN
    RETURN NEW;
  END IF;
  body := jsonb_build_object('task_id', NEW.id, 'title', NEW.title, 'repo', NEW.repo);
  INSERT INTO world_events (type, agent_id, task_id, actor_user_id, payload)
    VALUES ('task.assigned', NEW.assigned_agent_id, NEW.id, NEW.user_id, body)
    RETURNING seq INTO new_seq;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
    VALUES (NEW.assigned_agent_id, 'task.assigned', new_seq, body);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_tasks_world_assigned ON agent_tasks;
CREATE TRIGGER agent_tasks_world_assigned AFTER INSERT OR UPDATE OF assigned_agent_id, assignee_user_id ON agent_tasks
  FOR EACH ROW EXECUTE FUNCTION world_task_assigned();
