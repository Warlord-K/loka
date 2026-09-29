ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS pace TEXT NOT NULL DEFAULT 'normal';
ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_pace_check;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_pace_check CHECK (pace IN ('normal', 'quick', 'urgent'));
ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_quick_alone;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_quick_alone CHECK (pace <> 'quick' OR (NOT plan_first AND attempts = 1));
CREATE INDEX IF NOT EXISTS idx_agent_tasks_urgent ON agent_tasks (id) WHERE pace = 'urgent';
