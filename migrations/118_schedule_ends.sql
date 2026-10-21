ALTER TABLE schedules ADD COLUMN IF NOT EXISTS task_id UUID REFERENCES agent_tasks(id) ON DELETE CASCADE;
ALTER TABLE schedules ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_schedules_task ON schedules (task_id) WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_schedules_expires ON schedules (expires_at) WHERE expires_at IS NOT NULL;

CREATE OR REPLACE FUNCTION world_end_quest_schedules() RETURNS trigger AS $$
BEGIN
  IF NEW.status IN ('done', 'failed', 'cancelled') AND OLD.status IS DISTINCT FROM NEW.status THEN
    DELETE FROM schedules WHERE task_id = NEW.id;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_end_quest_schedules ON agent_tasks;
CREATE TRIGGER world_end_quest_schedules
  AFTER UPDATE OF status ON agent_tasks
  FOR EACH ROW EXECUTE FUNCTION world_end_quest_schedules();
