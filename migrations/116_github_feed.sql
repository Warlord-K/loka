ALTER TABLE users ADD COLUMN IF NOT EXISTS github_feed BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS github_feed_primed_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS github_feed_seen_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS github_seen (
  user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  notification_id  TEXT NOT NULL,
  updated_at       TIMESTAMPTZ NOT NULL,
  event_seq        BIGINT,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, notification_id, updated_at)
);

CREATE INDEX IF NOT EXISTS github_seen_created_at ON github_seen (user_id, created_at);

ALTER TABLE agent_subscriptions ADD COLUMN IF NOT EXISTS id UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE agent_subscriptions ADD COLUMN IF NOT EXISTS match JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE agent_subscriptions ADD COLUMN IF NOT EXISTS task_id UUID REFERENCES agent_tasks(id) ON DELETE CASCADE;
ALTER TABLE agent_subscriptions ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;
ALTER TABLE agent_subscriptions ADD COLUMN IF NOT EXISTS note TEXT;

ALTER TABLE agent_subscriptions DROP CONSTRAINT IF EXISTS agent_subscriptions_pkey;
ALTER TABLE agent_subscriptions ADD PRIMARY KEY (id);
ALTER TABLE agent_subscriptions DROP CONSTRAINT IF EXISTS agent_subscriptions_match_object;
ALTER TABLE agent_subscriptions ADD CONSTRAINT agent_subscriptions_match_object CHECK (jsonb_typeof(match) = 'object');

CREATE UNIQUE INDEX IF NOT EXISTS agent_subscriptions_unique
  ON agent_subscriptions (agent_id, event_type, match, COALESCE(task_id, '00000000-0000-0000-0000-000000000000'::uuid));
CREATE INDEX IF NOT EXISTS agent_subscriptions_routed ON agent_subscriptions (event_type) WHERE task_id IS NULL;
CREATE INDEX IF NOT EXISTS agent_subscriptions_task ON agent_subscriptions (task_id) WHERE task_id IS NOT NULL;

CREATE OR REPLACE FUNCTION world_route_subscriptions() RETURNS trigger AS $$
DECLARE
  subject_name TEXT;
  subject_owner UUID;
  event_owner UUID;
  event_channel UUID;
  channel_public BOOLEAN := TRUE;
BEGIN
  IF COALESCE(NEW.payload->>'quiet', '') = 'true' OR NEW.payload->>'run_kind' = 'turn' OR NEW.org_id IS NULL THEN
    RETURN NEW;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM agent_subscriptions s
    WHERE s.task_id IS NULL
      AND (s.event_type = NEW.type
           OR (right(s.event_type, 1) = '.' AND starts_with(NEW.type, s.event_type)
               AND NEW.type NOT IN ('agent.progress', 'run.activity')))
  ) THEN
    RETURN NEW;
  END IF;
  IF NEW.agent_id IS NOT NULL THEN
    SELECT name, owner_user_id INTO subject_name, subject_owner FROM agent_profiles WHERE id = NEW.agent_id;
  END IF;
  IF NEW.type = 'slack.notification' OR starts_with(NEW.type, 'github.') THEN
    event_owner := world_safe_uuid(NEW.payload->>'owner_user_id');
    IF event_owner IS NULL THEN
      RETURN NEW;
    END IF;
  ELSIF NEW.payload ? 'owner_user_id' THEN
    event_owner := world_safe_uuid(NEW.payload->>'owner_user_id');
  END IF;
  IF starts_with(NEW.type, 'channel.') AND NEW.payload ? 'channel_id' THEN
    event_channel := world_safe_uuid(NEW.payload->>'channel_id');
    SELECT kind = 'channel' INTO channel_public FROM channels WHERE id = event_channel;
    channel_public := COALESCE(channel_public, FALSE);
  END IF;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
  SELECT DISTINCT ON (s.agent_id) s.agent_id, NEW.type, NEW.seq,
         NEW.payload || jsonb_build_object(
           'subject_agent_id', NEW.agent_id,
           'subject_agent', subject_name,
           'subscribed', true)
         || CASE WHEN s.match <> '{}'::jsonb THEN jsonb_build_object('watch', s.match) ELSE '{}'::jsonb END
  FROM agent_subscriptions s
  JOIN agent_profiles p ON p.id = s.agent_id
  WHERE s.task_id IS NULL
    AND (s.event_type = NEW.type
         OR (right(s.event_type, 1) = '.' AND starts_with(NEW.type, s.event_type)
             AND NEW.type NOT IN ('agent.progress', 'run.activity')))
    AND s.match <@ NEW.payload
    AND (s.expires_at IS NULL OR s.expires_at > NOW())
    AND p.org_id = NEW.org_id
    AND s.agent_id IS DISTINCT FROM NEW.agent_id
    AND (NEW.agent_id IS NULL OR subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner)
    AND (event_owner IS NULL OR p.owner_user_id = event_owner)
    AND (event_channel IS NULL OR channel_public OR EXISTS (
      SELECT 1 FROM channel_members m
      WHERE m.channel_id = event_channel AND m.member_kind = 'agent' AND m.member_id = s.agent_id))
  ORDER BY s.agent_id, (s.match = '{}'::jsonb) DESC;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_pr_watch_key(url TEXT) RETURNS JSONB AS $$
DECLARE
  parts TEXT[];
BEGIN
  parts := regexp_match(COALESCE(url, ''), '^https://github\.com/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)/pull/([0-9]+)');
  IF parts IS NULL THEN
    RETURN NULL;
  END IF;
  RETURN jsonb_build_object('repo', lower(parts[1]), 'number', parts[2]::bigint);
END;
$$ LANGUAGE plpgsql IMMUTABLE;

CREATE OR REPLACE FUNCTION world_quest_pr_watch() RETURNS trigger AS $$
DECLARE
  key JSONB;
BEGIN
  IF TG_OP = 'UPDATE'
     AND NEW.pr_url IS NOT DISTINCT FROM OLD.pr_url
     AND NEW.assigned_agent_id IS NOT DISTINCT FROM OLD.assigned_agent_id
     AND NEW.status IS NOT DISTINCT FROM OLD.status THEN
    RETURN NEW;
  END IF;
  key := world_pr_watch_key(NEW.pr_url);
  IF NEW.status = 'cancelled' OR key IS NULL OR NEW.assigned_agent_id IS NULL THEN
    DELETE FROM agent_subscriptions WHERE task_id = NEW.id;
    RETURN NEW;
  END IF;
  IF TG_OP = 'UPDATE'
     AND NEW.pr_url IS NOT DISTINCT FROM OLD.pr_url
     AND NEW.assigned_agent_id IS NOT DISTINCT FROM OLD.assigned_agent_id THEN
    RETURN NEW;
  END IF;
  DELETE FROM agent_subscriptions WHERE task_id = NEW.id;
  INSERT INTO agent_subscriptions (agent_id, event_type, match, task_id, note)
  VALUES (NEW.assigned_agent_id, 'github.', key, NEW.id, 'pull request of this quest')
  ON CONFLICT DO NOTHING;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_quest_pr_watch ON agent_tasks;
CREATE TRIGGER world_quest_pr_watch
  AFTER INSERT OR UPDATE OF pr_url, assigned_agent_id, status ON agent_tasks
  FOR EACH ROW EXECUTE FUNCTION world_quest_pr_watch();

CREATE OR REPLACE FUNCTION world_quest_pr_unwatch() RETURNS trigger AS $$
BEGIN
  IF NEW.kind = 'merge' AND NEW.status IN ('done', 'failed', 'skipped')
     AND OLD.status IS DISTINCT FROM NEW.status THEN
    DELETE FROM agent_subscriptions WHERE task_id = NEW.task_id;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_quest_pr_unwatch ON task_milestones;
CREATE TRIGGER world_quest_pr_unwatch
  AFTER UPDATE OF status ON task_milestones
  FOR EACH ROW EXECUTE FUNCTION world_quest_pr_unwatch();

INSERT INTO agent_subscriptions (agent_id, event_type, match, task_id, note)
SELECT t.assigned_agent_id, 'github.', world_pr_watch_key(t.pr_url), t.id, 'pull request of this quest'
FROM agent_tasks t
JOIN task_milestones m ON m.task_id = t.id AND m.kind = 'merge' AND m.status = 'active'
WHERE t.assigned_agent_id IS NOT NULL AND t.status <> 'cancelled' AND world_pr_watch_key(t.pr_url) IS NOT NULL
ON CONFLICT DO NOTHING;
