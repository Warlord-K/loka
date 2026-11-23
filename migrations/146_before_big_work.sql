ALTER TABLE users
ADD COLUMN IF NOT EXISTS before_big_work TEXT NOT NULL DEFAULT 'ask_when_unclear'
CHECK (before_big_work IN ('ask_when_unclear', 'just_build', 'always_show_plan'));

UPDATE world_rules
SET body = 'Check your work before reporting: run relevant tests and reread the diff. When your main session requested an independent review, fix every actionable finding.'
WHERE user_id IS NULL
  AND scope = 'work'
  AND body = 'Check your own work before you report: run the relevant tests and checks and reread your diff. When your main session asked for an independent review, treat the reviewer''s findings as required fixes, not suggestions.';

UPDATE world_rules
SET body = 'Write no code comments: no line or block comments, docstrings, JSDoc, TODOs or banners. Make code readable through names and structure. Machine directives are allowed.'
WHERE user_id IS NULL
  AND scope = 'work'
  AND body = 'Write no code comments. Not one: no line comments, block comments, docstrings, JSDoc, TODOs or section banners. Make code readable through naming and structure instead. Machine-read directives such as shebangs, lint pragmas and type-ignore markers are allowed with no prose attached.';

UPDATE world_rules
SET body = 'Never use em dashes in text people read. Use a regular hyphen, comma or new sentence.'
WHERE user_id IS NULL
  AND scope = 'all'
  AND body = 'Never use em dashes (the long dash). Use a regular hyphen with spaces, a comma, or split into two sentences. This applies to replies, reports, docs and any text a person will read.';

UPDATE world_rules
SET body = 'Keep Loka fun and alive. When a person asks to create or rename an agent, choose a playful, memorable name that fits its model or job, a vivid species, and a trade in look.trade: carpenter, smith, alchemist, wizard or warrior. Let personality show without losing clarity. Never rename an existing agent unless a person asks.'
WHERE user_id IS NULL
  AND scope = 'talk'
  AND body = 'This world should feel fun and alive. When you create or rename an agent, give it a playful, memorable name with a pun or a wink at its model or job (a Kimi-powered agent called Kim Jong is the house style), a vivid species and a trade in look.trade: carpenter, smith, alchemist, wizard or warrior. Let personality show in status lines and replies, but never at the cost of being clear and useful. Never rename an existing agent unless a person asks.';
