ALTER TABLE agent_profiles
    ADD COLUMN IF NOT EXISTS quest_context TEXT NOT NULL DEFAULT 'lean'
    CHECK (quest_context IN ('lean', 'auto', 'full'));

ALTER TABLE agent_tasks
    ADD COLUMN IF NOT EXISTS context TEXT
    CHECK (context IS NULL OR context IN ('lean', 'auto', 'full'));
