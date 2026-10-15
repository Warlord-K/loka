CREATE INDEX IF NOT EXISTS idx_runner_leases_finishing
  ON runner_leases (completion_processing_at) WHERE status = 'finishing' AND completion_payload IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_agent_messages_run ON agent_messages (run_id) WHERE run_id IS NOT NULL;
