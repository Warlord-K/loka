ALTER TABLE agent_profiles
  ADD COLUMN IF NOT EXISTS tags TEXT[] NOT NULL DEFAULT '{}',
  ADD COLUMN IF NOT EXISTS expertise TEXT;

CREATE INDEX IF NOT EXISTS idx_agent_profiles_tags ON agent_profiles USING GIN (tags);

UPDATE agent_profiles p SET tags = sub.tags
FROM (
  SELECT assigned_agent_id AS id, ARRAY(
    SELECT DISTINCT unnest(ARRAY_AGG(kind) || ARRAY_AGG(repo) FILTER (WHERE repo IS NOT NULL))
  ) AS tags
  FROM agent_tasks
  WHERE assigned_agent_id IS NOT NULL AND status IN ('done', 'review')
  GROUP BY assigned_agent_id
) sub
WHERE p.id = sub.id AND cardinality(p.tags) = 0;
