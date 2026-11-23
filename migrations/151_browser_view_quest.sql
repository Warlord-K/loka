ALTER TABLE terminal_sessions ADD COLUMN IF NOT EXISTS task_id UUID REFERENCES agent_tasks(id) ON DELETE SET NULL;

ALTER TABLE terminal_sessions DROP CONSTRAINT IF EXISTS terminal_sessions_task;
ALTER TABLE terminal_sessions ADD CONSTRAINT terminal_sessions_task CHECK (task_id IS NULL OR kind = 'browser');
