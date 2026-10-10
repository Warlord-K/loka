CREATE TABLE IF NOT EXISTS agent_teams (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name           TEXT NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 60),
  slug           TEXT NOT NULL,
  purpose        TEXT NOT NULL DEFAULT '' CHECK (length(purpose) <= 2000),
  rules          TEXT NOT NULL DEFAULT '' CHECK (length(rules) <= 4000),
  lead_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  channel_id     UUID REFERENCES channels(id) ON DELETE SET NULL,
  created_by_agent_id UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_teams_owner_slug ON agent_teams (owner_user_id, slug);

CREATE TABLE IF NOT EXISTS agent_team_members (
  team_id      UUID NOT NULL REFERENCES agent_teams(id) ON DELETE CASCADE,
  member_kind  TEXT NOT NULL CHECK (member_kind IN ('agent', 'user')),
  member_id    UUID NOT NULL,
  added_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (team_id, member_kind, member_id)
);

CREATE INDEX IF NOT EXISTS idx_agent_team_members_member ON agent_team_members (member_kind, member_id);

CREATE OR REPLACE FUNCTION world_team_member_gone() RETURNS trigger AS $$
BEGIN
  DELETE FROM agent_team_members WHERE member_kind = 'agent' AND member_id = OLD.id;
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_profiles_team_member_gone ON agent_profiles;
CREATE TRIGGER agent_profiles_team_member_gone AFTER DELETE ON agent_profiles
  FOR EACH ROW EXECUTE FUNCTION world_team_member_gone();

ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS team_id UUID REFERENCES agent_teams(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_agent_tasks_team ON agent_tasks (team_id) WHERE team_id IS NOT NULL;

ALTER TABLE channels DROP CONSTRAINT IF EXISTS channels_kind_check;
ALTER TABLE channels ADD CONSTRAINT channels_kind_check CHECK (kind IN ('channel', 'dm', 'team'));
CREATE UNIQUE INDEX IF NOT EXISTS idx_channels_team_name ON channels (lower(name)) WHERE kind = 'team';

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
  IF NEW.team_id IS NOT NULL THEN
    body := body || jsonb_build_object(
      'team_id', NEW.team_id,
      'team', (SELECT name FROM agent_teams WHERE id = NEW.team_id),
      'team_lead', (SELECT lead_agent_id = NEW.assigned_agent_id FROM agent_teams WHERE id = NEW.team_id)
    );
  END IF;
  INSERT INTO world_events (type, agent_id, task_id, actor_user_id, payload)
    VALUES ('task.assigned', NEW.assigned_agent_id, NEW.id, NEW.user_id, body)
    RETURNING seq INTO new_seq;
  INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
    VALUES (NEW.assigned_agent_id, 'task.assigned', new_seq, body);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_team_feed() RETURNS trigger AS $$
DECLARE
  team RECORD;
  who TEXT;
  line TEXT;
  status TEXT;
  summary TEXT;
  msg_id BIGINT;
  feed_seq BIGINT;
BEGIN
  IF NEW.task_id IS NULL OR NEW.type NOT IN ('task.finished', 'task.merged', 'task.cancelled') THEN
    RETURN NEW;
  END IF;
  IF COALESCE((NEW.payload->>'quiet')::boolean, false) THEN
    RETURN NEW;
  END IF;
  SELECT tm.id, tm.name, tm.owner_user_id, tm.lead_agent_id, tm.channel_id, c.name AS channel_name,
         k.title, k.assigned_agent_id
    INTO team
    FROM agent_tasks k
    JOIN agent_teams tm ON tm.id = k.team_id
    LEFT JOIN channels c ON c.id = tm.channel_id
   WHERE k.id = NEW.task_id;
  IF NOT FOUND OR team.channel_id IS NULL THEN
    RETURN NEW;
  END IF;
  SELECT name INTO who FROM agent_profiles WHERE id = COALESCE(NEW.agent_id, team.assigned_agent_id);
  who := COALESCE(who, 'Someone');
  status := COALESCE(NULLIF(NEW.payload->>'status', ''), 'done');
  summary := left(COALESCE(NEW.payload->'report'->>'summary', ''), 400);
  line := CASE NEW.type
    WHEN 'task.finished' THEN
      CASE WHEN COALESCE(NEW.payload->>'error', '') <> '' THEN
        who || ' could not finish quest:' || NEW.task_id || E'\n' || left(NEW.payload->>'error', 300)
      ELSE
        who || ' finished quest:' || NEW.task_id || ' (' || status || ')'
          || CASE WHEN summary <> '' THEN E'\n' || summary ELSE '' END
          || CASE WHEN COALESCE(NEW.payload->>'pr_url', '') <> '' THEN E'\n' || (NEW.payload->>'pr_url') ELSE '' END
      END
    WHEN 'task.merged' THEN 'The pull request for quest:' || NEW.task_id || ' merged.'
    ELSE 'quest:' || NEW.task_id || ' was cancelled.'
  END;
  INSERT INTO channel_messages (channel_id, author_kind, author_agent_id, body, meta)
    VALUES (team.channel_id, 'system', NEW.agent_id, line,
            jsonb_build_object('kind', 'team_feed', 'team_id', team.id, 'task_id', NEW.task_id, 'event', NEW.type))
    RETURNING id INTO msg_id;
  INSERT INTO world_events (type, agent_id, task_id, payload)
    VALUES ('channel.message', NEW.agent_id, NEW.task_id, jsonb_build_object(
      'channel_id', team.channel_id, 'channel', '#' || team.channel_name, 'message_id', msg_id,
      'author', who, 'author_kind', 'system', 'author_agent_id', NEW.agent_id, 'body', line,
      'team_id', team.id, 'quiet', true))
    RETURNING seq INTO feed_seq;
  IF NEW.type = 'task.finished' AND team.lead_agent_id IS NOT NULL
     AND team.lead_agent_id IS DISTINCT FROM COALESCE(NEW.agent_id, team.assigned_agent_id) THEN
    INSERT INTO agent_inbox (agent_id, kind, event_seq, payload)
      VALUES (team.lead_agent_id, 'team.update', feed_seq, jsonb_build_object(
        'team_id', team.id, 'team', team.name, 'channel', team.channel_name, 'channel_id', team.channel_id,
        'task_id', NEW.task_id, 'title', team.title, 'member', who, 'status', status,
        'error', NEW.payload->>'error', 'summary', summary, 'pr_url', NEW.payload->>'pr_url'));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_events_team_feed ON world_events;
CREATE TRIGGER world_events_team_feed AFTER INSERT ON world_events
  FOR EACH ROW EXECUTE FUNCTION world_team_feed();
