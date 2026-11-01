ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;
ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS archive_delete_at TIMESTAMPTZ;

ALTER TABLE agent_profiles DROP CONSTRAINT IF EXISTS agent_profiles_archive_delete_needs_archive;
ALTER TABLE agent_profiles ADD CONSTRAINT agent_profiles_archive_delete_needs_archive
    CHECK (archive_delete_at IS NULL OR archived_at IS NOT NULL);

CREATE INDEX IF NOT EXISTS idx_agent_profiles_archive_delete ON agent_profiles (archive_delete_at) WHERE archive_delete_at IS NOT NULL;
