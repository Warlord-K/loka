DELETE FROM channel_members m
WHERE m.member_kind = 'agent'
  AND NOT EXISTS (SELECT 1 FROM agent_profiles a WHERE a.id = m.member_id);
