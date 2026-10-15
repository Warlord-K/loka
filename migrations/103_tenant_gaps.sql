UPDATE world_runners
   SET shared = false, shared_by = NULL, shared_org_id = NULL, updated_at = NOW()
 WHERE shared AND shared_by IS DISTINCT FROM owner_user_id;

ALTER TABLE world_runners DROP CONSTRAINT IF EXISTS world_runners_shared_by_owner;
ALTER TABLE world_runners ADD CONSTRAINT world_runners_shared_by_owner
  CHECK (NOT shared OR shared_by = owner_user_id);

CREATE OR REPLACE FUNCTION world_runner_share_scope() RETURNS trigger AS $$
BEGIN
  IF pg_trigger_depth() > 1 AND NEW.shared AND OLD.shared_org_id IS NOT NULL AND NEW.shared_org_id IS NULL THEN
    NEW.shared := false;
    NEW.shared_by := NULL;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_runner_share_scope ON world_runners;
CREATE TRIGGER world_runner_share_scope BEFORE UPDATE OF shared_org_id ON world_runners
  FOR EACH ROW EXECUTE FUNCTION world_runner_share_scope();

ALTER TABLE users ADD COLUMN IF NOT EXISTS token_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN IF NOT EXISTS disabled_at TIMESTAMPTZ;
