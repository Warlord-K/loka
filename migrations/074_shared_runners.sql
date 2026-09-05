ALTER TABLE world_runners ADD COLUMN IF NOT EXISTS shared BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE world_runners ADD COLUMN IF NOT EXISTS shared_by UUID REFERENCES users(id) ON DELETE SET NULL;
CREATE INDEX IF NOT EXISTS idx_world_runners_shared ON world_runners (last_seen_at DESC) WHERE shared AND status = 'online';
