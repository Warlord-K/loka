CREATE TABLE IF NOT EXISTS world_rules (
  id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id     UUID REFERENCES users(id) ON DELETE CASCADE,
  scope       TEXT NOT NULL DEFAULT 'all' CHECK (scope IN ('all', 'talk', 'work')),
  body        TEXT NOT NULL,
  enabled     BOOLEAN NOT NULL DEFAULT TRUE,
  position    INTEGER NOT NULL DEFAULT 0,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_world_rules_user ON world_rules (user_id, position);

ALTER TABLE agent_sessions ADD COLUMN IF NOT EXISTS rules_hash TEXT;

INSERT INTO world_rules (user_id, scope, body, position) VALUES
  (NULL, 'work', 'Every change gets an independent review before it ships. Treat the reviewer''s findings as required fixes, not suggestions.', 10),
  (NULL, 'work', 'Write no code comments. Not one: no line comments, block comments, docstrings, JSDoc, TODOs or section banners. Make code readable through naming and structure instead. Machine-read directives such as shebangs, lint pragmas and type-ignore markers are allowed with no prose attached.', 20),
  (NULL, 'talk', 'Reply concisely, in simple ELI5 language. No fluff, no filler, no apologies.', 30);
