ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS revoked_powers TEXT[] NOT NULL DEFAULT '{}';

ALTER TABLE users ADD COLUMN IF NOT EXISTS main_agent_id UUID REFERENCES agent_profiles(id) ON DELETE SET NULL;

UPDATE users u
   SET main_agent_id = a.id
  FROM (
    SELECT DISTINCT ON (owner_user_id) id, owner_user_id
      FROM agent_profiles
     WHERE role = 'assistant' AND owner_user_id IS NOT NULL
     ORDER BY owner_user_id, created_at
  ) a
 WHERE a.owner_user_id = u.id AND u.main_agent_id IS NULL;

UPDATE agent_profiles SET role = 'worker' WHERE role = 'assistant';
