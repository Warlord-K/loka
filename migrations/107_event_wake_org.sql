CREATE OR REPLACE FUNCTION world_notify_event() RETURNS trigger AS $$
BEGIN
  PERFORM pg_notify('world_events', json_build_object(
    'seq', NEW.seq, 'type', NEW.type, 'agent_id', NEW.agent_id, 'task_id', NEW.task_id, 'org_id', NEW.org_id)::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
