-- 1.N2: deregistration. The row is kept (jobs, leases, attempts and blobs reference it) but the key is
-- detached, so later signed calls with that key are unknown (401) and the key may enrol afresh with the secret.
ALTER TABLE devices ALTER COLUMN public_key DROP NOT NULL;
ALTER TABLE devices ADD COLUMN deregistered_at BIGINT;
