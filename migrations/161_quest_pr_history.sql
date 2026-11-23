ALTER TABLE agent_tasks ADD COLUMN IF NOT EXISTS past_prs JSONB NOT NULL DEFAULT '[]'::jsonb;

ALTER TABLE agent_tasks DROP CONSTRAINT IF EXISTS agent_tasks_past_prs_array;
ALTER TABLE agent_tasks ADD CONSTRAINT agent_tasks_past_prs_array CHECK (jsonb_typeof(past_prs) = 'array');

CREATE OR REPLACE FUNCTION world_quest_pr_replaced() RETURNS trigger AS $$
DECLARE
  merge_status TEXT;
BEGIN
  IF OLD.pr_url IS NULL OR NEW.pr_url IS NULL OR NEW.pr_url = OLD.pr_url THEN
    RETURN NEW;
  END IF;
  SELECT status INTO merge_status FROM task_milestones
   WHERE task_id = NEW.id AND kind = 'merge'
   ORDER BY created_at DESC LIMIT 1;
  IF NOT EXISTS (SELECT 1 FROM jsonb_array_elements(NEW.past_prs) p WHERE p->>'url' = OLD.pr_url) THEN
    NEW.past_prs := NEW.past_prs || jsonb_build_array(jsonb_build_object(
      'url', OLD.pr_url,
      'number', OLD.pr_number,
      'state', CASE merge_status WHEN 'done' THEN 'merged' WHEN 'failed' THEN 'closed' END,
      'replaced_at', NOW()));
  END IF;
  UPDATE task_milestones
     SET status = 'pending', detail = NULL, done_at = NULL, next_check_at = NULL, data = '{}'::jsonb, updated_at = NOW()
   WHERE task_id = NEW.id AND kind = 'merge' AND status IN ('done', 'failed');
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_quest_pr_replaced ON agent_tasks;
CREATE TRIGGER world_quest_pr_replaced BEFORE UPDATE OF pr_url ON agent_tasks
FOR EACH ROW EXECUTE FUNCTION world_quest_pr_replaced();
