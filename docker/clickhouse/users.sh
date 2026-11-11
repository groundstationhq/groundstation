#!/bin/bash
# One ClickHouse user per service, each with its own password and only the
# role it needs. The api grants the roles their privileges and creates the row
# policy when it migrates; the roles are created here too so the users can be
# granted them first. Runs once, when the data volume is new.
#
# GS_INGEST_PASSWORD, GS_API_PASSWORD and GS_WORKER_PASSWORD come from
# GROUNDSTATION_{INGEST,API,WORKER}_CLICKHOUSE_PASSWORD: set them before the
# first start when self-hosting.
set -euo pipefail

# A password as a SQL string literal's contents.
quoted() { printf %s "$1" | sed -e 's/\\/\\\\/g' -e "s/'/\\\\'/g"; }

clickhouse client --user "$CLICKHOUSE_USER" --password "$CLICKHOUSE_PASSWORD" --multiquery <<SQL
CREATE ROLE IF NOT EXISTS gs_writer;
CREATE ROLE IF NOT EXISTS gs_reader;
CREATE USER IF NOT EXISTS gs_ingest IDENTIFIED WITH sha256_password BY '$(quoted "$GS_INGEST_PASSWORD")' DEFAULT ROLE NONE;
CREATE USER IF NOT EXISTS gs_api IDENTIFIED WITH sha256_password BY '$(quoted "$GS_API_PASSWORD")' DEFAULT ROLE NONE;
CREATE USER IF NOT EXISTS gs_worker IDENTIFIED WITH sha256_password BY '$(quoted "$GS_WORKER_PASSWORD")' DEFAULT ROLE NONE;
GRANT gs_writer TO gs_ingest;
GRANT gs_reader TO gs_api, gs_worker;
ALTER USER gs_ingest DEFAULT ROLE gs_writer;
ALTER USER gs_api DEFAULT ROLE gs_reader;
ALTER USER gs_worker DEFAULT ROLE gs_reader;
SQL
