ALTER TABLE world_reports ADD COLUMN IF NOT EXISTS challenge_id UUID REFERENCES world_items(id) ON DELETE SET NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_world_reports_challenge ON world_reports (challenge_id) WHERE challenge_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_world_items_practice_source ON world_items (owner_user_id, org_id, (data->>'source_report'))
  WHERE kind = 'challenge' AND status <> 'archived' AND data ? 'source_report';
CREATE INDEX IF NOT EXISTS idx_world_items_owner_kind ON world_items (owner_user_id, kind, status) WHERE kind IN ('challenge', 'adventure');

CREATE TEMP TABLE academy_courses_164 AS
SELECT gen_random_uuid() AS id,
       g.owner_user_id,
       g.org_id,
       g.subject,
       g.created_at,
       (SELECT p.agent_id FROM world_practice_coach_prefs p
         JOIN agent_profiles a ON a.id = p.agent_id AND a.archived_at IS NULL
         WHERE p.user_id = g.owner_user_id AND p.subject = g.subject) AS coach
FROM (
  SELECT r.owner_user_id, r.org_id, r.practice->>'subject' AS subject, MIN(r.created_at) AS created_at
  FROM world_reports r
  WHERE r.practice IS NOT NULL AND r.kind = 'canvas' AND r.challenge_id IS NULL
  GROUP BY r.owner_user_id, r.org_id, r.practice->>'subject'
) g;

CREATE TEMP TABLE academy_lessons_164 AS
SELECT gen_random_uuid() AS id,
       r.id AS report_id,
       c.id AS course_id,
       r.owner_user_id,
       r.org_id,
       r.title,
       r.author_agent_id,
       r.task_id,
       r.created_at,
       c.subject,
       CASE WHEN r.practice->>'coach_agent_id' ~ '^[0-9a-f-]{36}$' THEN r.practice->>'coach_agent_id' END AS coach,
       ROW_NUMBER() OVER (PARTITION BY c.id ORDER BY r.created_at, r.id) AS ord
FROM world_reports r
JOIN academy_courses_164 c
  ON c.owner_user_id = r.owner_user_id AND c.org_id IS NOT DISTINCT FROM r.org_id AND c.subject = r.practice->>'subject'
WHERE r.practice IS NOT NULL AND r.kind = 'canvas' AND r.challenge_id IS NULL;

INSERT INTO world_items (id, kind, name, owner_user_id, org_id, data, created_at, updated_at)
SELECT c.id, 'adventure', initcap(replace(c.subject, '-', ' ')), c.owner_user_id, c.org_id,
       jsonb_build_object(
         'story', '',
         'challenges', (SELECT COALESCE(jsonb_agg(l.id ORDER BY l.ord), '[]'::jsonb) FROM academy_lessons_164 l WHERE l.course_id = c.id),
         'folder', 'Practice',
         'skill', c.subject,
         'coach_agent_id', c.coach,
         'from_practice', true),
       c.created_at, NOW()
FROM academy_courses_164 c;

INSERT INTO world_items (id, kind, name, owner_user_id, org_id, created_by_agent, data, created_at, updated_at)
SELECT l.id, 'challenge', l.title, l.owner_user_id, l.org_id, l.author_agent_id,
       jsonb_build_object(
         'type', 'canvas',
         'prompt', '',
         'hints', '[]'::jsonb,
         'skill', l.subject,
         'difficulty', 'medium',
         'xp', 40,
         'minutes', 15,
         'tests', NULL,
         'reading', '[]'::jsonb,
         'coach_agent_id', l.coach,
         'adventure_id', l.course_id::text,
         'order', l.ord,
         'report_id', l.report_id::text,
         'task_id', l.task_id::text),
       l.created_at, NOW()
FROM academy_lessons_164 l;

UPDATE world_reports r SET challenge_id = l.id FROM academy_lessons_164 l WHERE r.id = l.report_id;

INSERT INTO challenge_attempts (id, user_id, challenge_id, answer, status, score, feedback, xp_awarded, grader_agent_id, created_at, graded_at)
SELECT a.id,
       a.user_id,
       l.id,
       COALESCE(NULLIF(btrim(a.note), ''), 'Canvas attempt ' || a.n),
       CASE
         WHEN a.status = 'checked' AND (jsonb_typeof(a."check"->'score') IS DISTINCT FROM 'number' OR (a."check"->>'score')::numeric >= 60) THEN 'passed'
         WHEN a.status = 'checked' THEN 'failed'
         WHEN a.n = MAX(a.n) FILTER (WHERE a.status = 'waiting') OVER (PARTITION BY a.report_id, a.user_id) THEN 'pending'
         ELSE 'error'
       END,
       CASE WHEN jsonb_typeof(a."check"->'score') = 'number' THEN LEAST(100, GREATEST(0, round((a."check"->>'score')::numeric)))::int END,
       CASE
         WHEN a.status = 'checked' THEN concat_ws(E'\n\n', 'Right: ' || (a."check"->>'right'), 'Fix: ' || (a."check"->>'fix'), 'Next: ' || (a."check"->>'next'))
         WHEN a.n <> MAX(a.n) FILTER (WHERE a.status = 'waiting') OVER (PARTITION BY a.report_id, a.user_id) THEN 'Replaced by a newer attempt.'
       END,
       0,
       a.coach_agent_id,
       a.created_at,
       CASE WHEN a.status = 'checked' THEN a.checked_at
            WHEN a.n <> MAX(a.n) FILTER (WHERE a.status = 'waiting') OVER (PARTITION BY a.report_id, a.user_id) THEN a.created_at END
FROM world_practice_attempts a
JOIN academy_lessons_164 l ON l.report_id = a.report_id
ON CONFLICT (id) DO NOTHING;

UPDATE challenge_attempts ca SET xp_awarded = 40
FROM (
  SELECT DISTINCT ON (x.challenge_id, x.user_id) x.id
  FROM challenge_attempts x
  JOIN academy_lessons_164 l ON l.id = x.challenge_id
  WHERE x.status = 'passed'
  ORDER BY x.challenge_id, x.user_id, x.created_at, x.id
) first_pass
WHERE ca.id = first_pass.id;

WITH gained AS (
  SELECT ca.user_id, l.subject AS skill, SUM(ca.xp_awarded)::int AS xp
  FROM challenge_attempts ca
  JOIN academy_lessons_164 l ON l.id = ca.challenge_id
  WHERE ca.xp_awarded > 0
  GROUP BY ca.user_id, l.subject
), per_user AS (
  SELECT user_id, SUM(xp)::int AS xp, jsonb_object_agg(skill, xp) AS skills FROM gained GROUP BY user_id
)
INSERT INTO user_progress (user_id, xp, level, skills)
SELECT user_id, xp, 1 + floor(sqrt(xp / 50.0))::int, skills FROM per_user
ON CONFLICT (user_id) DO UPDATE SET
  xp = user_progress.xp + EXCLUDED.xp,
  level = 1 + floor(sqrt((user_progress.xp + EXCLUDED.xp) / 50.0))::int,
  skills = (
    SELECT COALESCE(jsonb_object_agg(k, COALESCE((user_progress.skills->>k)::numeric, 0)::int + COALESCE((EXCLUDED.skills->>k)::numeric, 0)::int), '{}'::jsonb)
    FROM (SELECT jsonb_object_keys(user_progress.skills) UNION SELECT jsonb_object_keys(EXCLUDED.skills)) keys(k)
  ),
  updated_at = NOW();

WITH finished AS (
  SELECT c.owner_user_id AS user_id, jsonb_agg(jsonb_build_object('id', 'trophy:' || c.id, 'kind', 'decoration', 'name', initcap(replace(c.subject, '-', ' ')) || ' trophy', 'at', NOW())) AS trophies
  FROM academy_courses_164 c
  WHERE NOT EXISTS (
    SELECT 1 FROM academy_lessons_164 l
    WHERE l.course_id = c.id
      AND NOT EXISTS (SELECT 1 FROM challenge_attempts a WHERE a.challenge_id = l.id AND a.user_id = c.owner_user_id AND a.status = 'passed')
  )
  GROUP BY c.owner_user_id
)
UPDATE user_progress p SET unlocks = p.unlocks || f.trophies, updated_at = NOW()
FROM finished f WHERE p.user_id = f.user_id;

UPDATE user_progress p SET unlocks = p.unlocks || COALESCE((
    SELECT jsonb_agg(jsonb_build_object('id', e.id, 'kind', e.kind, 'name', e.name, 'at', NOW()))
    FROM (
      SELECT u.id, u.kind, u.name FROM (VALUES
        ('lantern-post', 'decoration', 'Lantern post', 2),
        ('scholar-cap', 'gear', 'Scholar''s cap', 3),
        ('reading-bench', 'decoration', 'Reading bench', 4),
        ('observatory', 'house', 'Observatory dome', 5),
        ('golden-quill', 'gear', 'Golden quill', 7),
        ('starlit-tower', 'house', 'Starlit tower', 10)
      ) u(id, kind, name, lvl)
      WHERE p.level >= u.lvl
      UNION ALL
      SELECT 'banner:' || s.key, 'decoration', initcap(replace(s.key, '-', ' ')) || ' banner'
      FROM jsonb_each(p.skills) s
      WHERE jsonb_typeof(s.value) = 'number' AND (s.value::text)::numeric >= 100
    ) e
    WHERE NOT EXISTS (SELECT 1 FROM jsonb_array_elements(p.unlocks) have WHERE have->>'id' = e.id)
  ), '[]'::jsonb), updated_at = NOW()
WHERE p.user_id IN (SELECT DISTINCT owner_user_id FROM academy_lessons_164);

DROP TABLE academy_lessons_164;
DROP TABLE academy_courses_164;
