ALTER TABLE world_events
  ADD COLUMN IF NOT EXISTS quest_scope JSONB,
  ADD COLUMN IF NOT EXISTS quest_tx BIGINT,
  ADD COLUMN IF NOT EXISTS related_task_id UUID;

CREATE OR REPLACE FUNCTION world_event_related_task_id(direct_task UUID, direct_run UUID, direct_session UUID, event_payload JSONB) RETURNS UUID AS $$
  WITH event_run AS (
    SELECT task_id, session_id
    FROM session_runs
    WHERE id = COALESCE(direct_run, world_safe_uuid(event_payload->>'run_id'))
  ), event_session AS (
    SELECT task_id
    FROM agent_sessions
    WHERE id = COALESCE(
      direct_session,
      (SELECT session_id FROM event_run),
      world_safe_uuid(event_payload->>'session_id')
    )
  )
  SELECT COALESCE(
    direct_task,
    (SELECT task_id FROM event_run),
    (SELECT task_id FROM event_session),
    world_safe_uuid(event_payload->>'task_id')
  );
$$ LANGUAGE SQL STABLE;

CREATE OR REPLACE FUNCTION world_store_event_related_task() RETURNS trigger AS $$
DECLARE
  previous_task UUID;
BEGIN
  IF TG_OP = 'UPDATE' THEN
    previous_task := OLD.task_id;
  END IF;
  NEW.related_task_id := COALESCE(
    NEW.related_task_id,
    NEW.task_id,
    previous_task,
    world_event_related_task_id(NULL, NEW.run_id, NEW.session_id, NEW.payload)
  );
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_store_event_related_task ON world_events;
CREATE TRIGGER world_store_event_related_task
BEFORE INSERT OR UPDATE OF task_id, run_id, session_id, payload ON world_events
FOR EACH ROW EXECUTE FUNCTION world_store_event_related_task();

CREATE INDEX IF NOT EXISTS idx_world_events_quest_card_txid
ON world_events (quest_tx, type, (payload->>'task_id'))
WHERE type IN ('task.changed', 'task.deleted');

CREATE OR REPLACE FUNCTION world_quest_card_scope_for_subject(organization UUID, has_subject BOOLEAN, subject_public BOOLEAN, subject_owner UUID, creator UUID, assignee UUID, task_source TEXT) RETURNS JSONB AS $$
DECLARE
  people JSONB;
BEGIN
  IF COALESCE(subject_public, FALSE) THEN
    RETURN jsonb_build_object('audience_org', true);
  END IF;
  SELECT COALESCE(jsonb_agg(person ORDER BY person), '[]'::jsonb) INTO people
  FROM (
    SELECT creator AS person WHERE creator IS NOT NULL AND EXISTS (SELECT 1 FROM users WHERE id = creator)
    UNION
    SELECT assignee
    WHERE NOT has_subject AND COALESCE(task_source, '') <> 'chat' AND assignee IS NOT NULL
      AND EXISTS (SELECT 1 FROM users WHERE id = assignee)
      AND EXISTS (SELECT 1 FROM org_members WHERE org_id = organization AND user_id = assignee AND role IN ('owner', 'admin', 'member'))
    UNION
    SELECT subject_owner WHERE subject_owner IS NOT NULL AND EXISTS (SELECT 1 FROM users WHERE id = subject_owner)
  ) eligible;
  RETURN jsonb_build_object('audience', people);
END;
$$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION world_quest_card_scope(organization UUID, subject UUID, creator UUID, assignee UUID, task_source TEXT) RETURNS JSONB AS $$
DECLARE
  subject_public BOOLEAN := FALSE;
  subject_owner UUID;
BEGIN
  IF subject IS NOT NULL THEN
    SELECT visibility = 'public', owner_user_id INTO subject_public, subject_owner
    FROM agent_profiles WHERE id = subject AND org_id = organization;
  END IF;
  RETURN world_quest_card_scope_for_subject(
    organization,
    subject IS NOT NULL,
    subject_public,
    subject_owner,
    creator,
    assignee,
    task_source
  );
END;
$$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION world_quest_scope_union(first_scope JSONB, last_scope JSONB) RETURNS JSONB AS $$
  SELECT CASE
    WHEN COALESCE(first_scope->>'audience_org', 'false') = 'true'
      OR COALESCE(last_scope->>'audience_org', 'false') = 'true'
    THEN jsonb_build_object('audience_org', true)
    ELSE jsonb_build_object(
      'audience',
      COALESCE((
        SELECT jsonb_agg(person ORDER BY person)
        FROM (
          SELECT value AS person FROM jsonb_array_elements(COALESCE(first_scope->'audience', '[]'::jsonb))
          UNION
          SELECT value FROM jsonb_array_elements(COALESCE(last_scope->'audience', '[]'::jsonb))
        ) people
      ), '[]'::jsonb)
    )
  END;
$$ LANGUAGE SQL IMMUTABLE;

CREATE OR REPLACE FUNCTION world_quest_scope_empty(scope JSONB) RETURNS BOOLEAN AS $$
  SELECT COALESCE(scope->>'audience_org', 'false') <> 'true'
    AND jsonb_array_length(COALESCE(scope->'audience', '[]'::jsonb)) = 0;
$$ LANGUAGE SQL IMMUTABLE;

CREATE OR REPLACE FUNCTION world_quest_scope_without_viewer(scope JSONB, viewer UUID) RETURNS JSONB AS $$
  SELECT CASE WHEN scope IS NULL THEN NULL ELSE
    (scope - 'audience' - 'audience_exclude')
    || CASE WHEN scope ? 'audience' THEN jsonb_build_object(
      'audience', COALESCE((
        SELECT jsonb_agg(person ORDER BY person)
        FROM jsonb_array_elements(scope->'audience') AS people(person)
        WHERE person <> to_jsonb(viewer::text)
      ), '[]'::jsonb)
    ) ELSE '{}'::jsonb END
    || CASE WHEN scope ? 'audience_exclude' THEN jsonb_build_object(
      'audience_exclude', COALESCE((
        SELECT jsonb_agg(person ORDER BY person)
        FROM jsonb_array_elements(scope->'audience_exclude') AS people(person)
        WHERE person <> to_jsonb(viewer::text)
      ), '[]'::jsonb)
    ) ELSE '{}'::jsonb END
  END;
$$ LANGUAGE SQL IMMUTABLE;

CREATE OR REPLACE FUNCTION world_quest_event_scope_allows(event_scope JSONB, viewer UUID) RETURNS BOOLEAN AS $$
  SELECT viewer IS NOT NULL
    AND (
      COALESCE(event_scope->'delivery'->>'audience_org', 'false') = 'true'
      OR COALESCE(event_scope->'delivery'->'audience', '[]'::jsonb) ? viewer::text
    )
    AND NOT (COALESCE(event_scope->'delivery'->'audience_exclude', '[]'::jsonb) ? viewer::text);
$$ LANGUAGE SQL IMMUTABLE;

CREATE OR REPLACE FUNCTION world_quest_event_scope_allows_in(event_scope JSONB, viewer UUID, organization UUID) RETURNS BOOLEAN AS $$
  SELECT world_quest_event_scope_allows(event_scope, viewer)
    AND EXISTS (SELECT 1 FROM org_members WHERE org_id = organization AND user_id = viewer);
$$ LANGUAGE SQL STABLE;

CREATE OR REPLACE FUNCTION world_quest_event_detail_allows(event_scope JSONB, viewer UUID) RETURNS BOOLEAN AS $$
  SELECT world_quest_event_scope_allows(
    jsonb_build_object('delivery', COALESCE(event_scope->'detail', event_scope->'delivery')),
    viewer
  );
$$ LANGUAGE SQL IMMUTABLE;

CREATE OR REPLACE FUNCTION world_quest_event_detail_allows_in(event_scope JSONB, viewer UUID, organization UUID) RETURNS BOOLEAN AS $$
  SELECT world_quest_event_detail_allows(event_scope, viewer)
    AND EXISTS (SELECT 1 FROM org_members WHERE org_id = organization AND user_id = viewer);
$$ LANGUAGE SQL STABLE;

CREATE OR REPLACE FUNCTION world_quest_changed(quest UUID, what TEXT, before_scope JSONB, attach_task BOOLEAN) RETURNS void AS $$
DECLARE
  q RECORD;
  final_scope JSONB;
BEGIN
  IF quest IS NULL THEN
    RETURN;
  END IF;
  SELECT id, org_id, assigned_agent_id, user_id, assignee_user_id, source INTO q FROM agent_tasks WHERE id = quest;
  IF NOT FOUND THEN
    RETURN;
  END IF;
  final_scope := world_quest_card_scope(q.org_id, q.assigned_agent_id, q.user_id, q.assignee_user_id, q.source);
  UPDATE world_events
  SET org_id = q.org_id,
      agent_id = q.assigned_agent_id,
      actor_user_id = NULL,
      payload = jsonb_build_object('task_id', q.id, 'quiet', true),
      quest_scope = jsonb_build_object(
        'initial', COALESCE(quest_scope->'initial', before_scope),
        'delivery', world_quest_scope_union(COALESCE(quest_scope->'initial', before_scope), final_scope)
      )
  WHERE quest_tx = txid_current() AND type = 'task.changed' AND payload->>'task_id' = quest::text;
  IF FOUND THEN
    IF attach_task THEN
      UPDATE world_events
      SET task_id = q.id
      WHERE quest_tx = txid_current() AND type = 'task.changed' AND payload->>'task_id' = quest::text AND task_id IS NULL;
    END IF;
    RETURN;
  END IF;
  INSERT INTO world_events (type, agent_id, task_id, org_id, payload, quest_scope, quest_tx)
    VALUES (
      'task.changed',
      q.assigned_agent_id,
      CASE WHEN attach_task THEN q.id ELSE NULL END,
      q.org_id,
      jsonb_build_object('task_id', q.id, 'quiet', true),
      jsonb_build_object(
        'initial', before_scope,
        'delivery', world_quest_scope_union(before_scope, final_scope)
      ),
      txid_current()
    );
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_quest_changed(quest UUID, what TEXT, before_scope JSONB) RETURNS void AS $$
BEGIN
  PERFORM world_quest_changed(quest, what, before_scope, true);
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_quest_changed(quest UUID, what TEXT) RETURNS void AS $$
DECLARE
  q RECORD;
BEGIN
  SELECT org_id, assigned_agent_id, user_id, assignee_user_id, source INTO q FROM agent_tasks WHERE id = quest;
  IF NOT FOUND THEN
    RETURN;
  END IF;
  PERFORM world_quest_changed(
    quest,
    what,
    world_quest_card_scope(q.org_id, q.assigned_agent_id, q.user_id, q.assignee_user_id, q.source)
  );
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_task_card_changed() RETURNS trigger AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND (NEW.status, NEW.title, NEW.repo, NEW.pr_url, NEW.result_content, NEW.assigned_agent_id, NEW.assignee_user_id, NEW.plan_first, NEW.attempts, NEW.pace)
       IS NOT DISTINCT FROM (OLD.status, OLD.title, OLD.repo, OLD.pr_url, OLD.result_content, OLD.assigned_agent_id, OLD.assignee_user_id, OLD.plan_first, OLD.attempts, OLD.pace) THEN
    RETURN NEW;
  END IF;
  IF TG_OP = 'INSERT' THEN
    PERFORM world_quest_changed(NEW.id, 'created', jsonb_build_object('audience', '[]'::jsonb));
  ELSE
    PERFORM world_quest_changed(
      NEW.id,
      CASE WHEN NEW.status IS DISTINCT FROM OLD.status THEN 'status' ELSE 'details' END,
      world_quest_card_scope(OLD.org_id, OLD.assigned_agent_id, OLD.user_id, OLD.assignee_user_id, OLD.source)
    );
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_agent_quest_audience_changed() RETURNS trigger AS $$
DECLARE
  quest RECORD;
BEGIN
  PERFORM id FROM agent_profiles WHERE id = NEW.id FOR UPDATE;
  FOR quest IN
    SELECT id, org_id, user_id, assignee_user_id, source
    FROM agent_tasks
    WHERE assigned_agent_id = NEW.id AND org_id = NEW.org_id
  LOOP
    PERFORM world_quest_changed(
      quest.id,
      'details',
      world_quest_card_scope_for_subject(
        quest.org_id,
        true,
        OLD.visibility = 'public',
        OLD.owner_user_id,
        quest.user_id,
        quest.assignee_user_id,
        quest.source
      ),
      false
    );
  END LOOP;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_agent_quest_audience_changed ON agent_profiles;
CREATE TRIGGER world_agent_quest_audience_changed
AFTER UPDATE OF visibility, owner_user_id ON agent_profiles
FOR EACH ROW
WHEN ((OLD.visibility, OLD.owner_user_id) IS DISTINCT FROM (NEW.visibility, NEW.owner_user_id))
EXECUTE FUNCTION world_agent_quest_audience_changed();

CREATE OR REPLACE FUNCTION world_emit_task_deleted(quest agent_tasks, detail JSONB) RETURNS void AS $$
DECLARE
  final_scope JSONB;
  initial_scope JSONB;
  delivery_scope JSONB;
  had_initial BOOLEAN := FALSE;
  legacy_event BIGINT;
BEGIN
  IF quest.org_id IS NULL OR NOT EXISTS (SELECT 1 FROM orgs WHERE id = quest.org_id) THEN
    RETURN;
  END IF;
  IF EXISTS (
    SELECT 1 FROM world_events
    WHERE quest_tx = txid_current() AND type = 'task.deleted' AND payload->>'task_id' = quest.id::text
  ) THEN
    RETURN;
  END IF;
  SELECT seq INTO legacy_event
  FROM world_events
  WHERE quest_tx IS NULL
    AND type = 'task.deleted'
    AND payload->>'task_id' = quest.id::text
    AND created_at = transaction_timestamp()
  ORDER BY seq DESC
  LIMIT 1;
  IF NOT EXISTS (SELECT 1 FROM users WHERE id = quest.user_id) THEN
    DELETE FROM world_events
    WHERE (quest_tx = txid_current() OR (quest_tx IS NULL AND created_at = transaction_timestamp()))
      AND type = 'task.changed'
      AND payload->>'task_id' = quest.id::text
    RETURNING quest_scope->'initial' INTO initial_scope;
    had_initial := FOUND;
    initial_scope := world_quest_scope_without_viewer(COALESCE(initial_scope, jsonb_build_object('audience', '[]'::jsonb)), quest.user_id);
    final_scope := world_quest_card_scope(quest.org_id, quest.assigned_agent_id, NULL, quest.assignee_user_id, quest.source);
    final_scope := world_quest_scope_without_viewer(final_scope, quest.user_id);
    delivery_scope := world_quest_scope_union(initial_scope, final_scope);
    IF NOT had_initial AND quest.assigned_agent_id IS NULL AND world_quest_scope_empty(delivery_scope) THEN
      DELETE FROM world_events WHERE seq = legacy_event;
      RETURN;
    END IF;
    IF legacy_event IS NOT NULL THEN
      UPDATE world_events
      SET agent_id = NULL,
          actor_user_id = NULL,
          org_id = quest.org_id,
          payload = jsonb_build_object('task_id', quest.id, 'quiet', true),
          quest_scope = jsonb_build_object('delivery', delivery_scope),
          quest_tx = txid_current()
      WHERE seq = legacy_event;
    ELSE
      INSERT INTO world_events (type, org_id, payload, quest_scope, quest_tx)
        VALUES (
          'task.deleted',
          quest.org_id,
          jsonb_build_object('task_id', quest.id, 'quiet', true),
          jsonb_build_object('delivery', delivery_scope),
          txid_current()
        );
    END IF;
    RETURN;
  END IF;
  final_scope := world_quest_card_scope(quest.org_id, quest.assigned_agent_id, quest.user_id, quest.assignee_user_id, quest.source);
  DELETE FROM world_events
  WHERE (quest_tx = txid_current() OR (quest_tx IS NULL AND created_at = transaction_timestamp()))
    AND type = 'task.changed'
    AND payload->>'task_id' = quest.id::text
  RETURNING quest_scope->'initial' INTO initial_scope;
  delivery_scope := world_quest_scope_union(COALESCE(initial_scope, final_scope), final_scope);
  IF legacy_event IS NOT NULL THEN
    UPDATE world_events
    SET agent_id = quest.assigned_agent_id,
        actor_user_id = NULL,
        org_id = quest.org_id,
        payload = CASE
          WHEN detail IS NULL THEN payload || jsonb_build_object('task_id', quest.id)
          ELSE detail || jsonb_build_object('task_id', quest.id)
        END,
        quest_scope = jsonb_build_object('creator_user_id', quest.user_id, 'delivery', delivery_scope, 'detail', final_scope),
        quest_tx = txid_current()
    WHERE seq = legacy_event;
  ELSE
    INSERT INTO world_events (type, agent_id, org_id, payload, quest_scope, quest_tx)
      VALUES (
        'task.deleted',
        quest.assigned_agent_id,
        quest.org_id,
        COALESCE(detail, '{}'::jsonb) || jsonb_build_object('task_id', quest.id),
        jsonb_build_object('creator_user_id', quest.user_id, 'delivery', delivery_scope, 'detail', final_scope),
        txid_current()
      );
  END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION world_task_deleted() RETURNS trigger AS $$
BEGIN
  PERFORM world_emit_task_deleted(OLD, NULL);
  RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_task_deleted ON agent_tasks;
CREATE TRIGGER world_task_deleted AFTER DELETE ON agent_tasks
FOR EACH ROW EXECUTE FUNCTION world_task_deleted();

CREATE OR REPLACE FUNCTION world_route_subscriptions() RETURNS trigger AS $$
DECLARE
  subject_name TEXT;
  subject_owner UUID;
  subject_visibility TEXT;
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
      AND (NEW.type <> 'slack.watched' OR s.match <> '{}'::jsonb)
  ) THEN
    RETURN NEW;
  END IF;
  IF NEW.agent_id IS NOT NULL THEN
    SELECT name, owner_user_id, visibility INTO subject_name, subject_owner, subject_visibility FROM agent_profiles WHERE id = NEW.agent_id;
  END IF;
  IF starts_with(NEW.type, 'slack.') OR starts_with(NEW.type, 'github.') THEN
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
    AND (NEW.type <> 'slack.watched' OR s.match <> '{}'::jsonb)
    AND s.match <@ NEW.payload
    AND (s.expires_at IS NULL OR s.expires_at > NOW())
    AND p.org_id = NEW.org_id
    AND s.agent_id IS DISTINCT FROM NEW.agent_id
    AND (
      (NEW.quest_scope IS NULL AND (NEW.agent_id IS NULL OR subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner))
      OR (NEW.quest_scope IS NOT NULL
        AND world_quest_event_detail_allows_in(NEW.quest_scope, p.owner_user_id, NEW.org_id)
        AND (NEW.agent_id IS NULL
          OR (subject_visibility = 'public' AND (subject_owner IS NULL OR p.owner_user_id IS NULL OR p.owner_user_id = subject_owner))
          OR (subject_visibility = 'private' AND subject_owner IS NOT NULL AND p.owner_user_id = subject_owner)))
    )
    AND (
      NEW.type <> 'task.deleted'
      OR NEW.quest_scope IS NOT NULL
      OR NEW.task_id IS NULL
      OR NOT EXISTS (SELECT 1 FROM agent_tasks legacy_quest WHERE legacy_quest.id = NEW.task_id)
      OR EXISTS (
        SELECT 1
        FROM agent_tasks legacy_quest
        WHERE legacy_quest.id = NEW.task_id
          AND legacy_quest.org_id = NEW.org_id
          AND world_quest_event_scope_allows_in(
            jsonb_build_object(
              'delivery',
              world_quest_card_scope(
                legacy_quest.org_id,
                legacy_quest.assigned_agent_id,
                legacy_quest.user_id,
                legacy_quest.assignee_user_id,
                legacy_quest.source
              )
            ),
            p.owner_user_id,
            NEW.org_id
          )
      )
    )
    AND (event_owner IS NULL OR p.owner_user_id = event_owner)
    AND (event_channel IS NULL OR channel_public OR EXISTS (
      SELECT 1 FROM channel_members m
      WHERE m.channel_id = event_channel AND m.member_kind = 'agent' AND m.member_id = s.agent_id))
  ORDER BY s.agent_id, (s.match = '{}'::jsonb) DESC;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
