CREATE TABLE insights (
    id                uuid PRIMARY KEY,
    tenant_id         uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    detector          text NOT NULL,
    detector_version  integer NOT NULL,
    -- Hash of the detector and what the finding is about (a trajectory, a
    -- file, a tool category): re-running a detector updates its findings.
    fingerprint       text NOT NULL,
    severity          text NOT NULL CHECK (severity IN ('low', 'medium', 'high')),
    title             text NOT NULL,
    body              text NOT NULL,
    recommendation    text NOT NULL,
    evidence          jsonb NOT NULL,
    metrics           jsonb NOT NULL,
    first_seen_at     timestamptz NOT NULL,
    last_seen_at      timestamptz NOT NULL,
    state             text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'dismissed', 'resolved')),
    -- The severity when someone last changed the state. A dismissed or
    -- resolved finding opens again only if it gets worse than this.
    state_severity    text,
    state_changed_by  uuid REFERENCES users (id) ON DELETE SET NULL,
    state_changed_at  timestamptz,
    UNIQUE (tenant_id, fingerprint)
);
CREATE INDEX insights_tenant_state ON insights (tenant_id, state, last_seen_at DESC);

-- How far each detector has looked at each tenant's events.
CREATE TABLE insight_runs (
    tenant_id    uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    detector     text NOT NULL,
    watermark    timestamptz NOT NULL,
    last_run_at  timestamptz NOT NULL,
    PRIMARY KEY (tenant_id, detector)
);
