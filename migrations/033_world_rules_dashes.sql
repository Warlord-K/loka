INSERT INTO world_rules (user_id, scope, body, position)
SELECT NULL, 'all', 'Never use em dashes (the long dash). Use a regular hyphen with spaces, a comma, or split into two sentences. This applies to replies, reports, docs and any text a person will read.', 40
WHERE NOT EXISTS (SELECT 1 FROM world_rules WHERE body LIKE 'Never use em dashes%');
