CREATE TABLE users (
    id          uuid PRIMARY KEY,
    email       text,
    name        text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- How a user signs in: an OpenID Connect issuer and its subject, or
-- ('local', 'admin') for the admin password. Users are matched by identity,
-- never by email, so another provider's account with the same address can't
-- take a user over.
CREATE TABLE identities (
    issuer   text NOT NULL,
    subject  text NOT NULL,
    user_id  uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (issuer, subject)
);

CREATE TABLE memberships (
    tenant_id   uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        text NOT NULL CHECK (role IN ('admin', 'member')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);
CREATE INDEX memberships_user ON memberships (user_id);

-- Session ids are stored only as hashes.
CREATE TABLE sessions (
    id_hash     bytea PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    tenant_id   uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX sessions_expires ON sessions (expires_at);

-- Sign-ins in progress with an OpenID Connect provider, keyed by the hash of
-- the `state` sent to it. Each is used once.
CREATE TABLE login_flows (
    state_hash     bytea PRIMARY KEY,
    pkce_verifier  text NOT NULL,
    nonce          text NOT NULL,
    return_to      text NOT NULL,
    expires_at     timestamptz NOT NULL
);

ALTER TABLE ingest_tokens
    ADD CONSTRAINT ingest_tokens_created_by FOREIGN KEY (created_by) REFERENCES users (id) ON DELETE SET NULL;
