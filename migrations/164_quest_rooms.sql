ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS asking TEXT NOT NULL DEFAULT 'off';
ALTER TABLE agent_profiles DROP CONSTRAINT IF EXISTS agent_profiles_asking_check;
ALTER TABLE agent_profiles ADD CONSTRAINT agent_profiles_asking_check CHECK (asking IN ('off', 'ask_agent', 'crew'));
ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS crew_variants JSONB NOT NULL DEFAULT '[]';
ALTER TABLE agent_profiles DROP CONSTRAINT IF EXISTS agent_profiles_crew_variants_check;
ALTER TABLE agent_profiles ADD CONSTRAINT agent_profiles_crew_variants_check
  CHECK (jsonb_typeof(crew_variants) = 'array' AND jsonb_array_length(crew_variants) <= 4);

ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS asking TEXT;
ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS crew_size SMALLINT;
ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS crew_variants JSONB;
ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_crew_variants_check;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_crew_variants_check
  CHECK (crew_variants IS NULL OR (jsonb_typeof(crew_variants) = 'array' AND jsonb_array_length(crew_variants) <= 4));
ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_asking_check;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_asking_check CHECK (asking IS NULL OR asking IN ('off', 'ask_agent', 'crew'));
ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_crew_size_check;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_crew_size_check CHECK (crew_size IS NULL OR crew_size BETWEEN 2 AND 4);

CREATE TABLE IF NOT EXISTS quest_room_members (
  quest_id      UUID NOT NULL REFERENCES agent_tasks(id) ON DELETE CASCADE,
  member_kind   TEXT NOT NULL CHECK (member_kind IN ('agent', 'person', 'session')),
  member_id     UUID NOT NULL,
  role          TEXT NOT NULL DEFAULT 'guest' CHECK (role IN ('owner', 'lead', 'helper', 'guest')),
  added_by      UUID,
  added_by_kind TEXT NOT NULL DEFAULT 'loka' CHECK (added_by_kind IN ('agent', 'person', 'loka')),
  added_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (quest_id, member_kind, member_id)
);

CREATE INDEX IF NOT EXISTS idx_quest_room_members_member ON quest_room_members (member_kind, member_id);

ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'chat';
ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS asked_by_session UUID REFERENCES agent_sessions(id) ON DELETE SET NULL;
ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS answered_by JSONB;
ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS routes_to JSONB;
ALTER TABLE agent_messages ADD COLUMN IF NOT EXISTS decision_status TEXT;
ALTER TABLE agent_messages DROP CONSTRAINT IF EXISTS agent_messages_kind_check;
ALTER TABLE agent_messages ADD CONSTRAINT agent_messages_kind_check
  CHECK (kind IN ('chat', 'question', 'answer', 'decision', 'needs_you', 'assumed'));
ALTER TABLE agent_messages DROP CONSTRAINT IF EXISTS agent_messages_decision_status_check;
ALTER TABLE agent_messages ADD CONSTRAINT agent_messages_decision_status_check
  CHECK (decision_status IS NULL OR (kind = 'question' AND decision_status IN ('open', 'answered', 'passed_up', 'assumed', 'parked')));

CREATE INDEX IF NOT EXISTS idx_agent_messages_room_kinds ON agent_messages ((meta->>'task_id'), created_at)
  WHERE kind <> 'chat';

ALTER TABLE world_rules ADD COLUMN IF NOT EXISTS model JSONB;
ALTER TABLE world_rules DROP CONSTRAINT IF EXISTS world_rules_model_check;
ALTER TABLE world_rules ADD CONSTRAINT world_rules_model_check CHECK (model IS NULL OR jsonb_typeof(model) = 'object');
