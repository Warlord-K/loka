ALTER TABLE world_rules DROP CONSTRAINT IF EXISTS world_rules_scope_check;
ALTER TABLE world_rules ADD CONSTRAINT world_rules_scope_check CHECK (scope IN ('all', 'talk', 'work', 'allow'));
