CREATE TABLE IF NOT EXISTS orgs (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  slug          TEXT NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,38}[a-z0-9]$'),
  name          TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 80),
  join_domains  TEXT[] NOT NULL DEFAULT '{}',
  settings      JSONB NOT NULL DEFAULT '{}'::jsonb,
  plan          TEXT NOT NULL DEFAULT 'free',
  created_by    UUID REFERENCES users(id) ON DELETE SET NULL,
  suspended_at  TIMESTAMPTZ,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS orgs_join_domains_idx ON orgs USING GIN (join_domains);
CREATE INDEX IF NOT EXISTS orgs_created_by_idx ON orgs (created_by, created_at DESC);

CREATE TABLE IF NOT EXISTS org_members (
  org_id         UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role           TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'guest')),
  main_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  invited_by     UUID REFERENCES users(id) ON DELETE SET NULL,
  joined_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (org_id, user_id)
);

CREATE INDEX IF NOT EXISTS org_members_user_idx ON org_members (user_id, joined_at);
CREATE UNIQUE INDEX IF NOT EXISTS org_members_main_agent_idx ON org_members (main_agent_id) WHERE main_agent_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS org_audit (
  id             BIGSERIAL PRIMARY KEY,
  org_id         UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  actor_user_id  UUID REFERENCES users(id) ON DELETE SET NULL,
  action         TEXT NOT NULL,
  target         TEXT,
  detail         JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS org_audit_org_idx ON org_audit (org_id, id DESC);

ALTER TABLE users ADD COLUMN IF NOT EXISTS active_org_id UUID REFERENCES orgs(id) ON DELETE SET NULL;
ALTER TABLE users ADD COLUMN IF NOT EXISTS instance_admin BOOLEAN NOT NULL DEFAULT FALSE;

CREATE OR REPLACE FUNCTION world_default_org() RETURNS UUID AS $$
  SELECT '00000000-0000-0000-0000-000000000001'::uuid
$$ LANGUAGE sql IMMUTABLE;

CREATE OR REPLACE FUNCTION world_safe_uuid(value TEXT) RETURNS UUID AS $$
BEGIN
  IF value ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
    RETURN value::uuid;
  END IF;
  RETURN NULL;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

CREATE OR REPLACE FUNCTION world_ladder_role(member_role TEXT) RETURNS TEXT AS $$
  SELECT CASE member_role
    WHEN 'owner' THEN 'admin'
    WHEN 'admin' THEN 'admin'
    WHEN 'member' THEN 'write'
    WHEN 'guest' THEN 'read'
  END
$$ LANGUAGE sql IMMUTABLE;

CREATE OR REPLACE FUNCTION world_active_org(person UUID) RETURNS UUID AS $$
  SELECT u.active_org_id FROM users u
  WHERE u.id = person
    AND EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = u.active_org_id AND m.user_id = person)
$$ LANGUAGE sql STABLE;

CREATE OR REPLACE FUNCTION world_only_org() RETURNS UUID AS $$
  SELECT CASE WHEN (SELECT COUNT(*) FROM orgs) = 1 THEN (SELECT id FROM orgs LIMIT 1) END
$$ LANGUAGE sql STABLE;

INSERT INTO orgs (id, slug, name)
VALUES (world_default_org(), 'loka', 'Loka')
ON CONFLICT (id) DO NOTHING;

INSERT INTO org_members (org_id, user_id, role, main_agent_id)
SELECT world_default_org(), u.id,
       CASE u.world_role WHEN 'admin' THEN 'admin' WHEN 'write' THEN 'member' ELSE 'guest' END,
       u.main_agent_id
FROM users u
WHERE u.world_role IS NOT NULL
ON CONFLICT (org_id, user_id) DO NOTHING;

UPDATE org_members SET role = 'owner'
WHERE org_id = world_default_org()
  AND user_id = (
    SELECT u.id FROM users u
    WHERE u.world_role = 'admin'
    ORDER BY (u.id = '00000000-0000-0000-0000-000000000000'::uuid) DESC,
             EXISTS (SELECT 1 FROM world_invites i WHERE i.accepted_by = u.id),
             u.email NULLS LAST
    LIMIT 1
  );

UPDATE users SET active_org_id = world_default_org() WHERE world_role IS NOT NULL AND active_org_id IS NULL;
UPDATE users SET instance_admin = TRUE WHERE world_role = 'admin';

CREATE OR REPLACE FUNCTION world_sync_member_role() RETURNS trigger AS $$
DECLARE
  person UUID := COALESCE(NEW.user_id, OLD.user_id);
BEGIN
  UPDATE users SET world_role = (
    SELECT world_ladder_role(m.role) FROM org_members m
    JOIN orgs o ON o.id = m.org_id AND o.suspended_at IS NULL
    WHERE m.user_id = person
    ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 0 WHEN 'member' THEN 1 ELSE 2 END
    LIMIT 1
  )
  WHERE id = person;
  RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_org_member_role ON org_members;
CREATE TRIGGER world_org_member_role
AFTER INSERT OR UPDATE OF role OR DELETE ON org_members
FOR EACH ROW EXECUTE FUNCTION world_sync_member_role();

ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE channels ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_events ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_reports ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_items ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_regions ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_todos ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_rules ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_skills ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_mcp_servers ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_uploads ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE api_tokens ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_runners ADD COLUMN IF NOT EXISTS shared_org_id UUID REFERENCES orgs(id) ON DELETE SET NULL;

UPDATE agent_profiles SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE channels SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE agent_tasks SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_events SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_reports SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_items SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_regions SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_todos SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_rules SET org_id = world_default_org() WHERE org_id IS NULL AND user_id IS NULL;
UPDATE world_skills SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_mcp_servers SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_uploads SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE api_tokens SET org_id = world_default_org() WHERE org_id IS NULL;

ALTER TABLE agent_profiles ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE channels ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE agent_tasks ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_reports ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_items ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_regions ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_todos ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_skills ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_mcp_servers ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE api_tokens ALTER COLUMN org_id SET NOT NULL;

CREATE INDEX IF NOT EXISTS agent_profiles_org_idx ON agent_profiles (org_id, created_at);
CREATE INDEX IF NOT EXISTS channels_org_idx ON channels (org_id, kind, name);
CREATE INDEX IF NOT EXISTS agent_tasks_org_idx ON agent_tasks (org_id, created_at DESC);
CREATE INDEX IF NOT EXISTS world_events_org_idx ON world_events (org_id, seq DESC);
CREATE INDEX IF NOT EXISTS world_reports_org_idx ON world_reports (org_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS world_items_org_idx ON world_items (org_id, kind, status);
CREATE INDEX IF NOT EXISTS world_regions_org_idx ON world_regions (org_id, status);
CREATE INDEX IF NOT EXISTS world_todos_org_idx ON world_todos (org_id, owner_user_id);

DROP INDEX IF EXISTS idx_channels_name;
CREATE UNIQUE INDEX IF NOT EXISTS idx_channels_org_name ON channels (org_id, lower(name)) WHERE kind = 'channel';
DROP INDEX IF EXISTS idx_channels_team_name;
CREATE UNIQUE INDEX IF NOT EXISTS idx_channels_org_team_name ON channels (org_id, lower(name)) WHERE kind = 'team';

ALTER TABLE world_invites ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
ALTER TABLE world_invites ADD COLUMN IF NOT EXISTS max_uses INTEGER CHECK (max_uses IS NULL OR max_uses BETWEEN 1 AND 1000);
ALTER TABLE world_invites ADD COLUMN IF NOT EXISTS uses INTEGER NOT NULL DEFAULT 0;
ALTER TABLE world_invites ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;
ALTER TABLE world_invites ALTER COLUMN email DROP NOT NULL;
ALTER TABLE world_invites DROP CONSTRAINT IF EXISTS world_invites_role_check;
UPDATE world_invites SET role = CASE role WHEN 'admin' THEN 'admin' WHEN 'write' THEN 'member' ELSE 'guest' END
WHERE role NOT IN ('admin', 'member', 'guest');
ALTER TABLE world_invites ADD CONSTRAINT world_invites_role_check CHECK (role IN ('admin', 'member', 'guest'));
UPDATE world_invites SET org_id = world_default_org() WHERE org_id IS NULL;
UPDATE world_invites SET uses = 1 WHERE accepted_at IS NOT NULL AND uses = 0;
ALTER TABLE world_invites ALTER COLUMN org_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS world_invites_org_idx ON world_invites (org_id, created_at DESC);

ALTER TABLE world_shared_environment_agents DROP CONSTRAINT IF EXISTS world_shared_environment_agents_name_fkey;
ALTER TABLE world_shared_environment ADD COLUMN IF NOT EXISTS org_id UUID REFERENCES orgs(id) ON DELETE CASCADE;
UPDATE world_shared_environment SET org_id = world_default_org() WHERE org_id IS NULL;
ALTER TABLE world_shared_environment ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_shared_environment DROP CONSTRAINT IF EXISTS world_shared_environment_pkey;
ALTER TABLE world_shared_environment ADD PRIMARY KEY (org_id, name);
ALTER TABLE world_shared_environment_agents ADD COLUMN IF NOT EXISTS org_id UUID;
UPDATE world_shared_environment_agents SET org_id = world_default_org() WHERE org_id IS NULL;
ALTER TABLE world_shared_environment_agents ALTER COLUMN org_id SET NOT NULL;
ALTER TABLE world_shared_environment_agents DROP CONSTRAINT IF EXISTS world_shared_environment_agents_pkey;
ALTER TABLE world_shared_environment_agents ADD PRIMARY KEY (org_id, name, agent_id);
ALTER TABLE world_shared_environment_agents ADD CONSTRAINT world_shared_environment_agents_key_fkey
  FOREIGN KEY (org_id, name) REFERENCES world_shared_environment(org_id, name) ON DELETE CASCADE;

CREATE OR REPLACE FUNCTION world_fill_agent_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.owner_user_id), world_only_org(), world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_agent_org ON agent_profiles;
CREATE TRIGGER world_agent_org BEFORE INSERT ON agent_profiles
FOR EACH ROW EXECUTE FUNCTION world_fill_agent_org();

CREATE OR REPLACE FUNCTION world_keep_agent_org() RETURNS trigger AS $$
BEGIN
  IF NEW.org_id IS DISTINCT FROM OLD.org_id THEN
    RAISE EXCEPTION 'an agent cannot move between organizations' USING ERRCODE = '23514';
  END IF;
  IF NEW.owner_user_id IS DISTINCT FROM OLD.owner_user_id AND NEW.owner_user_id IS NOT NULL
     AND NOT EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = NEW.org_id AND m.user_id = NEW.owner_user_id) THEN
    RAISE EXCEPTION 'the new owner is not in this organization' USING ERRCODE = '23514';
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_agent_org_fixed ON agent_profiles;
CREATE TRIGGER world_agent_org_fixed BEFORE UPDATE OF org_id, owner_user_id ON agent_profiles
FOR EACH ROW EXECUTE FUNCTION world_keep_agent_org();

CREATE OR REPLACE FUNCTION world_fill_channel_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.created_by), world_only_org(), world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_channel_org ON channels;
CREATE TRIGGER world_channel_org BEFORE INSERT ON channels
FOR EACH ROW EXECUTE FUNCTION world_fill_channel_org();

CREATE OR REPLACE FUNCTION world_fill_task_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(
    NEW.org_id,
    (SELECT org_id FROM agent_profiles WHERE id = NEW.assigned_agent_id),
    (SELECT org_id FROM agent_tasks WHERE id = NEW.parent_task_id),
    world_active_org(NEW.user_id),
    world_only_org(),
    world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_task_org ON agent_tasks;
CREATE TRIGGER world_task_org BEFORE INSERT ON agent_tasks
FOR EACH ROW EXECUTE FUNCTION world_fill_task_org();

CREATE OR REPLACE FUNCTION world_fill_report_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(
    NEW.org_id,
    (SELECT org_id FROM agent_profiles WHERE id = NEW.author_agent_id),
    (SELECT org_id FROM agent_tasks WHERE id = NEW.task_id),
    world_active_org(NEW.owner_user_id),
    world_only_org(),
    world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_report_org ON world_reports;
CREATE TRIGGER world_report_org BEFORE INSERT ON world_reports
FOR EACH ROW EXECUTE FUNCTION world_fill_report_org();

CREATE OR REPLACE FUNCTION world_fill_region_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(
    NEW.org_id,
    (SELECT org_id FROM agent_profiles WHERE id = NEW.created_by_agent),
    world_active_org(NEW.owner_user_id),
    world_only_org(),
    world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_region_org ON world_regions;
CREATE TRIGGER world_region_org BEFORE INSERT ON world_regions
FOR EACH ROW EXECUTE FUNCTION world_fill_region_org();

CREATE OR REPLACE FUNCTION world_fill_item_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(
    NEW.org_id,
    (SELECT org_id FROM world_regions WHERE id = NEW.region_id),
    (SELECT org_id FROM agent_profiles WHERE id = NEW.created_by_agent),
    world_active_org(NEW.owner_user_id),
    world_only_org(),
    world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_item_org ON world_items;
CREATE TRIGGER world_item_org BEFORE INSERT ON world_items
FOR EACH ROW EXECUTE FUNCTION world_fill_item_org();

CREATE OR REPLACE FUNCTION world_fill_todo_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(
    NEW.org_id,
    (SELECT org_id FROM agent_profiles WHERE id = NEW.added_by_agent),
    world_active_org(NEW.owner_user_id),
    world_only_org(),
    world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_todo_org ON world_todos;
CREATE TRIGGER world_todo_org BEFORE INSERT ON world_todos
FOR EACH ROW EXECUTE FUNCTION world_fill_todo_org();

CREATE OR REPLACE FUNCTION world_fill_kit_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.created_by), world_only_org(), world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_skill_org ON world_skills;
CREATE TRIGGER world_skill_org BEFORE INSERT ON world_skills
FOR EACH ROW EXECUTE FUNCTION world_fill_kit_org();
DROP TRIGGER IF EXISTS world_mcp_server_org ON world_mcp_servers;
CREATE TRIGGER world_mcp_server_org BEFORE INSERT ON world_mcp_servers
FOR EACH ROW EXECUTE FUNCTION world_fill_kit_org();

CREATE OR REPLACE FUNCTION world_fill_rule_org() RETURNS trigger AS $$
BEGIN
  IF NEW.user_id IS NULL THEN
    NEW.org_id := COALESCE(NEW.org_id, world_only_org(), world_default_org());
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_rule_org ON world_rules;
CREATE TRIGGER world_rule_org BEFORE INSERT ON world_rules
FOR EACH ROW EXECUTE FUNCTION world_fill_rule_org();

CREATE OR REPLACE FUNCTION world_fill_upload_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.uploaded_by), world_only_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_upload_org ON world_uploads;
CREATE TRIGGER world_upload_org BEFORE INSERT ON world_uploads
FOR EACH ROW EXECUTE FUNCTION world_fill_upload_org();

CREATE OR REPLACE FUNCTION world_fill_token_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.user_id), world_only_org(), world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_token_org ON api_tokens;
CREATE TRIGGER world_token_org BEFORE INSERT ON api_tokens
FOR EACH ROW EXECUTE FUNCTION world_fill_token_org();

CREATE OR REPLACE FUNCTION world_fill_invite_org() RETURNS trigger AS $$
BEGIN
  NEW.org_id := COALESCE(NEW.org_id, world_active_org(NEW.invited_by), world_only_org(), world_default_org());
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_invite_org ON world_invites;
CREATE TRIGGER world_invite_org BEFORE INSERT ON world_invites
FOR EACH ROW EXECUTE FUNCTION world_fill_invite_org();

CREATE OR REPLACE FUNCTION world_fill_event_org() RETURNS trigger AS $$
BEGIN
  IF NEW.org_id IS NULL AND NEW.agent_id IS NOT NULL THEN
    SELECT org_id INTO NEW.org_id FROM agent_profiles WHERE id = NEW.agent_id;
  END IF;
  IF NEW.org_id IS NULL AND NEW.task_id IS NOT NULL THEN
    SELECT org_id INTO NEW.org_id FROM agent_tasks WHERE id = NEW.task_id;
  END IF;
  IF NEW.org_id IS NULL AND NEW.session_id IS NOT NULL THEN
    SELECT a.org_id INTO NEW.org_id FROM agent_sessions s JOIN agent_profiles a ON a.id = s.agent_id WHERE s.id = NEW.session_id;
  END IF;
  IF NEW.org_id IS NULL AND NEW.run_id IS NOT NULL THEN
    SELECT a.org_id INTO NEW.org_id FROM session_runs r JOIN agent_profiles a ON a.id = r.agent_id WHERE r.id = NEW.run_id;
  END IF;
  IF NEW.org_id IS NULL AND NEW.payload ? 'channel_id' THEN
    SELECT org_id INTO NEW.org_id FROM channels WHERE id = world_safe_uuid(NEW.payload->>'channel_id');
  END IF;
  IF NEW.org_id IS NULL AND NEW.payload ? 'report_id' THEN
    SELECT org_id INTO NEW.org_id FROM world_reports WHERE id = world_safe_uuid(NEW.payload->>'report_id');
  END IF;
  IF NEW.org_id IS NULL AND NEW.payload ? 'region_id' THEN
    SELECT org_id INTO NEW.org_id FROM world_regions WHERE id = world_safe_uuid(NEW.payload->>'region_id');
  END IF;
  IF NEW.org_id IS NULL AND NEW.payload ? 'item_id' THEN
    SELECT org_id INTO NEW.org_id FROM world_items WHERE id = world_safe_uuid(NEW.payload->>'item_id');
  END IF;
  IF NEW.org_id IS NULL AND NEW.payload ? 'org_id' THEN
    SELECT id INTO NEW.org_id FROM orgs WHERE id = world_safe_uuid(NEW.payload->>'org_id');
  END IF;
  IF NEW.org_id IS NULL THEN
    NEW.org_id := COALESCE(
      world_active_org(NEW.actor_user_id),
      world_active_org(world_safe_uuid(NEW.payload->>'owner_user_id')),
      world_only_org());
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_event_org ON world_events;
CREATE TRIGGER world_event_org BEFORE INSERT ON world_events
FOR EACH ROW EXECUTE FUNCTION world_fill_event_org();

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
    WHERE s.event_type = NEW.type
       OR (right(s.event_type, 1) = '.' AND starts_with(NEW.type, s.event_type)
           AND NEW.type NOT IN ('agent.progress', 'run.activity'))
  ) THEN
    RETURN NEW;
  END IF;
  IF NEW.agent_id IS NOT NULL THEN
    SELECT name, owner_user_id INTO subject_name, subject_owner FROM agent_profiles WHERE id = NEW.agent_id;
  END IF;
  IF NEW.type = 'slack.notification' THEN
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
  FROM agent_subscriptions s
  JOIN agent_profiles p ON p.id = s.agent_id
  WHERE (s.event_type = NEW.type
         OR (right(s.event_type, 1) = '.' AND starts_with(NEW.type, s.event_type)
             AND NEW.type NOT IN ('agent.progress', 'run.activity')))
    AND p.org_id = NEW.org_id
    AND s.agent_id IS DISTINCT FROM NEW.agent_id
    AND (NEW.agent_id IS NULL OR subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner)
    AND (event_owner IS NULL OR p.owner_user_id = event_owner)
    AND (event_channel IS NULL OR channel_public OR EXISTS (
      SELECT 1 FROM channel_members m
      WHERE m.channel_id = event_channel AND m.member_kind = 'agent' AND m.member_id = s.agent_id))
  ORDER BY s.agent_id;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

INSERT INTO channels (name, topic, org_id)
SELECT 'general', 'Everyone, people and agents', o.id FROM orgs o
WHERE NOT EXISTS (SELECT 1 FROM channels c WHERE c.org_id = o.id AND c.kind = 'channel' AND lower(c.name) = 'general');
