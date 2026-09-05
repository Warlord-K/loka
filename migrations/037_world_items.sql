CREATE TABLE IF NOT EXISTS world_items (
  id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  kind              TEXT NOT NULL CHECK (kind IN ('building', 'decoration', 'critter', 'timer', 'list')),
  name              TEXT NOT NULL,
  owner_user_id     UUID REFERENCES users(id) ON DELETE CASCADE,
  created_by_agent  UUID REFERENCES agent_profiles(id) ON DELETE SET NULL,
  data              JSONB NOT NULL DEFAULT '{}'::jsonb,
  status            TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'done', 'archived')),
  fires_at          TIMESTAMPTZ,
  created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_world_items_kind ON world_items (kind, status);
CREATE INDEX IF NOT EXISTS idx_world_items_timers ON world_items (fires_at) WHERE kind = 'timer' AND status = 'active';

CREATE OR REPLACE FUNCTION world_notify_item_timer() RETURNS trigger AS $$
BEGIN
  IF NEW.kind = 'timer' THEN
    PERFORM pg_notify('schedules_changed', NEW.id::text);
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_items_timer_notify ON world_items;
CREATE TRIGGER world_items_timer_notify AFTER INSERT OR UPDATE OF fires_at, status ON world_items
  FOR EACH ROW EXECUTE FUNCTION world_notify_item_timer();
