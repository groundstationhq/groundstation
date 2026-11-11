#!/bin/bash
# One Postgres role each for ingest and the worker, so neither holds the
# owner's credentials. The api grants them what they need when it migrates
# (crates/backend/src/pg/migrate.rs): ingest can resolve tokens, the worker
# can read tenants and write findings. The api itself connects as the owner.
#
# Runs when the data volume is new. On an existing volume, or to change a
# password: set the new one in .env, recreate the container so it has it
# (`docker compose up -d postgres`; exec runs with the environment the
# container was created with), run the script, then `docker compose up -d`
# to restart the services with it (the api grants new roles their privileges
# when it starts):
#   docker compose exec postgres bash /docker-entrypoint-initdb.d/roles.sh
# Self-hosting, add `-p groundstation-selfhost --profile selfhost` to each.
#
# GS_INGEST_PASSWORD and GS_WORKER_PASSWORD come from
# GROUNDSTATION_INGEST_POSTGRES_PASSWORD and GROUNDSTATION_WORKER_POSTGRES_PASSWORD.
set -euo pipefail

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
	-v ingest="$GS_INGEST_PASSWORD" -v worker="$GS_WORKER_PASSWORD" <<'SQL'
SELECT 'CREATE ROLE gs_ingest' WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'gs_ingest') \gexec
SELECT 'CREATE ROLE gs_worker' WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'gs_worker') \gexec
ALTER ROLE gs_ingest LOGIN PASSWORD :'ingest';
ALTER ROLE gs_worker LOGIN PASSWORD :'worker';
SQL
