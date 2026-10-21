UPDATE users SET name = NULL
WHERE id = '00000000-0000-0000-0000-000000000000' AND name = 'Local User';

UPDATE users SET email = NULL
WHERE id = '00000000-0000-0000-0000-000000000000' AND email = 'local@karna.dev';
