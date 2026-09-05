ALTER TABLE schedules ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'routine';

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'schedules_kind_check') THEN
    ALTER TABLE schedules ADD CONSTRAINT schedules_kind_check CHECK (kind IN ('routine', 'heartbeat'));
  END IF;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS schedules_one_heartbeat ON schedules (agent_id) WHERE kind = 'heartbeat';
