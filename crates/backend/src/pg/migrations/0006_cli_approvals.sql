-- Computers approved for `groundstation cloud login` whose CLI hasn't yet
-- collected its token. The browser carries only a one-time code back to the
-- CLI, never the token, so no token is left in the browser's history.
CREATE TABLE cli_approvals (
    code_hash   bytea PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label       text NOT NULL,
    -- SHA-256, in hex, of a secret only the CLI that asked holds.
    challenge   text NOT NULL,
    expires_at  timestamptz NOT NULL
);
