CREATE TABLE IF NOT EXISTS world_regions (
  id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  owner_user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name              TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 80),
  slug              TEXT NOT NULL CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,59}$'),
  theme             TEXT NOT NULL DEFAULT 'meadow'
                    CHECK (theme IN ('meadow', 'forest', 'docks', 'academy', 'snow', 'desert', 'mountain', 'swamp', 'city', 'island')),
  seed              BIGINT NOT NULL DEFAULT floor(random() * 2147483647)::bigint,
  visibility        TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public')),
  status            TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
  created_by_agent  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  data              JSONB NOT NULL DEFAULT '{}'::jsonb,
  created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE (owner_user_id, slug)
);

CREATE INDEX IF NOT EXISTS idx_world_regions_owner ON world_regions (owner_user_id, status);

ALTER TABLE world_items ADD COLUMN IF NOT EXISTS region_id UUID REFERENCES world_regions(id) ON DELETE CASCADE;
CREATE INDEX IF NOT EXISTS idx_world_items_region ON world_items (region_id, status);

ALTER TABLE world_items DROP CONSTRAINT IF EXISTS world_items_kind_check;
ALTER TABLE world_items ADD CONSTRAINT world_items_kind_check
  CHECK (kind IN ('building', 'decoration', 'critter', 'timer', 'list', 'portal', 'npc', 'challenge', 'adventure'));

CREATE TABLE IF NOT EXISTS user_progress (
  user_id          UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  xp               INTEGER NOT NULL DEFAULT 0 CHECK (xp >= 0),
  level            INTEGER NOT NULL DEFAULT 1 CHECK (level >= 1),
  skills           JSONB NOT NULL DEFAULT '{}'::jsonb,
  streak           INTEGER NOT NULL DEFAULT 0 CHECK (streak >= 0),
  best_streak      INTEGER NOT NULL DEFAULT 0 CHECK (best_streak >= 0),
  last_active_day  DATE,
  unlocks          JSONB NOT NULL DEFAULT '[]'::jsonb,
  updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS challenge_attempts (
  id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  challenge_id     UUID NOT NULL REFERENCES world_items(id) ON DELETE CASCADE,
  answer           TEXT NOT NULL,
  status           TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'passed', 'failed', 'error')),
  score            INTEGER CHECK (score BETWEEN 0 AND 100),
  feedback         TEXT,
  xp_awarded       INTEGER NOT NULL DEFAULT 0,
  grader_agent_id  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  session_id       UUID REFERENCES agent_sessions(id) ON DELETE SET NULL,
  created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  graded_at        TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_challenge_attempts_user ON challenge_attempts (user_id, challenge_id, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_challenge_attempts_one_pending ON challenge_attempts (user_id, challenge_id) WHERE status = 'pending';

CREATE TABLE IF NOT EXISTS academy_offers (
  id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  agent_id      UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  task_id       UUID REFERENCES agent_tasks(id) ON DELETE SET NULL,
  status        TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'ready', 'expired')),
  challenge_id  UUID REFERENCES world_items(id) ON DELETE SET NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  ready_at      TIMESTAMPTZ
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_academy_offers_one_open ON academy_offers (user_id) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS idx_academy_offers_user ON academy_offers (user_id, created_at DESC);
