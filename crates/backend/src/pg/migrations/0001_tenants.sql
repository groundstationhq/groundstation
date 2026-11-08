CREATE TABLE tenants (
    id              uuid PRIMARY KEY,
    name            text NOT NULL,
    plan            text NOT NULL DEFAULT 'free',
    retention_days  integer NOT NULL CHECK (retention_days > 0),
    created_at      timestamptz NOT NULL DEFAULT now(),
    suspended_at    timestamptz
);

CREATE TABLE ingest_tokens (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    token_hash    bytea NOT NULL UNIQUE,
    label         text NOT NULL,
    created_by    uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz,
    revoked_at    timestamptz
);
CREATE INDEX ingest_tokens_tenant ON ingest_tokens (tenant_id);
