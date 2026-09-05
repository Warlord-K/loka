ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS review BOOLEAN NOT NULL DEFAULT TRUE;

UPDATE world_rules
SET body = 'Do not review your own work with sub-agents, review skills or slash commands. When your main session asked for a review, an independent reviewer checks the work after you report; treat its findings as required fixes, not suggestions.'
WHERE user_id IS NULL
  AND scope = 'work'
  AND body = 'Every change gets an independent review before it ships. Treat the reviewer''s findings as required fixes, not suggestions.';
