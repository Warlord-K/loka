ALTER TABLE agent_inbox ADD COLUMN IF NOT EXISTS retry_at TIMESTAMPTZ;

CREATE OR REPLACE FUNCTION world_inbox_retry_delay() RETURNS trigger AS $$
BEGIN
  IF NEW.status = 'pending' AND OLD.status = 'processing' THEN
    NEW.retry_at := NOW() + make_interval(secs => 60 * GREATEST(NEW.attempts, 1));
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS agent_inbox_retry_delay ON agent_inbox;
CREATE TRIGGER agent_inbox_retry_delay BEFORE UPDATE OF status ON agent_inbox
  FOR EACH ROW EXECUTE FUNCTION world_inbox_retry_delay();
