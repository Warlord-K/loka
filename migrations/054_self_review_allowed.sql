UPDATE world_rules
SET body = 'Check your own work before you report: run the relevant tests and checks and reread your diff. When your main session asked for an independent review, treat the reviewer''s findings as required fixes, not suggestions.'
WHERE user_id IS NULL
  AND scope = 'work'
  AND body = 'Do not review your own work with sub-agents, review skills or slash commands. When your main session asked for a review, an independent reviewer checks the work after you report; treat its findings as required fixes, not suggestions.';
