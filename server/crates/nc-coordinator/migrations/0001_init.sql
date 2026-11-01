-- Phase 0 schema (task 0.7): devices, replay protection, dummy tasks and leases.
-- Times are Unix seconds (BIGINT).

CREATE TABLE devices (
    id          UUID PRIMARY KEY,
    public_key  BYTEA NOT NULL UNIQUE CHECK (octet_length(public_key) = 32),
    account_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    platform    TEXT NOT NULL,
    created_at  BIGINT NOT NULL,
    last_seen   BIGINT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'idle'
);

-- Signed-request nonces, kept for the replay window.
CREATE TABLE request_nonces (
    public_key  BYTEA NOT NULL,
    nonce       TEXT NOT NULL,
    seen_at     BIGINT NOT NULL,
    PRIMARY KEY (public_key, nonce)
);
CREATE INDEX request_nonces_seen_at ON request_nonces (seen_at);

CREATE TABLE tasks (
    id                 UUID PRIMARY KEY,
    state              TEXT NOT NULL CHECK (state IN ('queued', 'leased', 'committed')),
    input              TEXT NOT NULL,
    result             TEXT,
    committed_attempt  UUID,
    attempts           INTEGER NOT NULL DEFAULT 0,
    created_at         BIGINT NOT NULL
);
CREATE INDEX tasks_state ON tasks (state);

CREATE TABLE leases (
    id          UUID PRIMARY KEY,
    task_id     UUID NOT NULL REFERENCES tasks (id),
    attempt_id  UUID NOT NULL UNIQUE,
    device_id   UUID NOT NULL REFERENCES devices (id),
    issued_at   BIGINT NOT NULL,
    expires_at  BIGINT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('active', 'completed', 'expired'))
);
CREATE INDEX leases_active ON leases (state, expires_at);
