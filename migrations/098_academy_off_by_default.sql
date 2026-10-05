ALTER TABLE users ALTER COLUMN academy_on SET DEFAULT false;
UPDATE users SET academy_on = false WHERE academy_on;
