CREATE TABLE IF NOT EXISTS agent_thread_reads (
  user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  agent_id       UUID NOT NULL REFERENCES agent_profiles(id) ON DELETE CASCADE,
  last_seen_seq  BIGINT NOT NULL DEFAULT 0,
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, agent_id)
);

INSERT INTO agent_thread_reads (user_id, agent_id, last_seen_seq)
SELECT u.id, a.id, (SELECT COALESCE(MAX(seq), 0) FROM world_events)
  FROM users u CROSS JOIN agent_profiles a
ON CONFLICT DO NOTHING;
