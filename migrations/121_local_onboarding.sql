UPDATE users SET onboarded_at = NULL, onboarding = NULL
WHERE id = '00000000-0000-0000-0000-000000000000'
  AND main_agent_id IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM agent_profiles WHERE owner_user_id = '00000000-0000-0000-0000-000000000000'
  );
