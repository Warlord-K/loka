CREATE INDEX IF NOT EXISTS idx_agent_messages_attachments ON agent_messages USING GIN ((meta->'attachments') jsonb_path_ops)
  WHERE meta ? 'attachments';
CREATE INDEX IF NOT EXISTS idx_channel_messages_attachments ON channel_messages USING GIN ((meta->'attachments') jsonb_path_ops)
  WHERE meta ? 'attachments';
CREATE INDEX IF NOT EXISTS idx_world_uploads_uploader ON world_uploads (uploaded_by);
