DROP INDEX IF EXISTS idx_world_runners_shared;
CREATE INDEX IF NOT EXISTS idx_world_runners_shared_status ON world_runners (status) WHERE shared;

CREATE INDEX IF NOT EXISTS idx_agent_inbox_open_ticks
    ON agent_inbox (agent_id, (payload->>'schedule_id'))
    WHERE kind = 'tick' AND status IN ('pending', 'processing');

WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (PARTITION BY agent_id, payload->>'schedule_id' ORDER BY id DESC) AS place
    FROM agent_inbox
    WHERE kind = 'tick' AND status = 'pending'
)
UPDATE agent_inbox i
SET status = 'done',
    done_at = NOW(),
    error = 'Skipped, a newer run of this schedule was already waiting'
FROM ranked r
WHERE i.id = r.id AND r.place > 1;
