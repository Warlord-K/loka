CREATE OR REPLACE FUNCTION world_fold_unfinished_usage() RETURNS trigger AS $$
DECLARE
  d_in BIGINT;
  d_cached BIGINT;
  d_out BIGINT;
BEGIN
  IF NEW.status IN ('queued', 'running', 'completed') THEN
    RETURN NEW;
  END IF;
  IF OLD.status IN ('queued', 'running') THEN
    d_in := NEW.input_tokens;
    d_cached := NEW.cached_input_tokens;
    d_out := NEW.output_tokens;
  ELSIF OLD.status = NEW.status THEN
    d_in := GREATEST(NEW.input_tokens - OLD.input_tokens, 0);
    d_cached := GREATEST(NEW.cached_input_tokens - OLD.cached_input_tokens, 0);
    d_out := GREATEST(NEW.output_tokens - OLD.output_tokens, 0);
  ELSE
    RETURN NEW;
  END IF;
  IF d_in + d_cached + d_out <= 0 THEN
    RETURN NEW;
  END IF;
  UPDATE agent_sessions s
     SET usage_baseline = (CASE WHEN jsonb_typeof(s.usage_baseline) = 'object' THEN s.usage_baseline ELSE '{}'::jsonb END)
       || jsonb_build_object(
            'input_tokens', COALESCE(floor((s.usage_baseline->>'input_tokens')::numeric)::bigint, 0) + d_in,
            'cached_input_tokens', COALESCE(floor((s.usage_baseline->>'cached_input_tokens')::numeric)::bigint, 0) + d_cached,
            'output_tokens', COALESCE(floor((s.usage_baseline->>'output_tokens')::numeric)::bigint, 0) + d_out)
   WHERE s.id = NEW.session_id
     AND s.backend = 'codex'
     AND s.cli_session_id IS NOT NULL
     AND jsonb_typeof(COALESCE(s.usage_baseline->'input_tokens', '0')) = 'number'
     AND jsonb_typeof(COALESCE(s.usage_baseline->'cached_input_tokens', '0')) = 'number'
     AND jsonb_typeof(COALESCE(s.usage_baseline->'output_tokens', '0')) = 'number';
  RETURN NEW;
END
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS world_fold_unfinished_usage ON session_runs;
CREATE TRIGGER world_fold_unfinished_usage
  AFTER UPDATE OF status, input_tokens, cached_input_tokens, output_tokens ON session_runs
  FOR EACH ROW EXECUTE FUNCTION world_fold_unfinished_usage();
