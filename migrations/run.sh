#!/bin/bash
set -euo pipefail

DB_USER="${POSTGRES_USER:-karna}"
DB_NAME="${POSTGRES_DB:-karna}"
DB_HOST="${POSTGRES_HOST:-postgres}"

psql="psql -h $DB_HOST -U $DB_USER -d $DB_NAME -v ON_ERROR_STOP=1"

$psql -c "
CREATE TABLE IF NOT EXISTS schema_migrations (
  version TEXT PRIMARY KEY,
  applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"

migration_count=$($psql -tAc "SELECT count(*) FROM schema_migrations")
has_tasks_table=$($psql -tAc "SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name='agent_tasks'" || echo "")

if [ "$migration_count" = "0" ] && [ "$has_tasks_table" = "1" ]; then
  echo "bootstrap: existing DB detected, detecting applied migrations"

  $psql -c "INSERT INTO schema_migrations (version) VALUES ('001_initial.sql')"
  echo "bootstrap: marked 001_initial.sql"

  has_password=$($psql -tAc "SELECT 1 FROM information_schema.columns WHERE table_name='users' AND column_name='password'" || echo "")
  if [ "$has_password" = "1" ]; then
    $psql -c "INSERT INTO schema_migrations (version) VALUES ('002_add_password.sql')"
    echo "bootstrap: marked 002_add_password.sql"
  fi

  has_parent=$($psql -tAc "SELECT 1 FROM information_schema.columns WHERE table_name='agent_tasks' AND column_name='parent_task_id'" || echo "")
  if [ "$has_parent" = "1" ]; then
    $psql -c "INSERT INTO schema_migrations (version) VALUES ('003_subtasks.sql')"
    echo "bootstrap: marked 003_subtasks.sql"
  fi

  has_cli=$($psql -tAc "SELECT 1 FROM information_schema.columns WHERE table_name='agent_tasks' AND column_name='cli'" || echo "")
  if [ "$has_cli" = "1" ]; then
    $psql -c "INSERT INTO schema_migrations (version) VALUES ('004_cli_model.sql')"
    echo "bootstrap: marked 004_cli_model.sql"
  fi
fi

for f in /migrations/[0-9]*.sql; do
  version=$(basename "$f")
  already_applied=$($psql -tAc "SELECT 1 FROM schema_migrations WHERE version = '$version'")
  if [ "$already_applied" = "1" ]; then
    echo "skip: $version (already applied)"
  else
    echo "applying: $version"
    record="INSERT INTO schema_migrations (version) VALUES ('$version')"
    if head -n 1 "$f" | grep -q '^-- no-transaction'; then
      $psql -f "$f"
      $psql -c "$record"
    else
      $psql --single-transaction -f "$f" -c "$record"
    fi
    echo "applied: $version"
  fi
done

echo "migrations complete"
