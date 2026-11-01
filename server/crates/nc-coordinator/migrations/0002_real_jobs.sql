-- Forward-only extension: existing Phase 0 dummy tasks remain valid.
ALTER TABLE devices ADD COLUMN heartbeat_at BIGINT;
CREATE TABLE jobs (
 id UUID PRIMARY KEY,
 requester UUID NOT NULL REFERENCES devices(id),
 manifest BYTEA NOT NULL CHECK (octet_length(manifest) <= 66000),
 deadline BIGINT NOT NULL,
 validation_count INTEGER NOT NULL CHECK (validation_count IN (1,2)),
 output_cap BIGINT NOT NULL,
 created_at BIGINT NOT NULL
);
ALTER TABLE tasks ADD COLUMN job_id UUID REFERENCES jobs(id);
ALTER TABLE tasks DROP CONSTRAINT tasks_state_check;
ALTER TABLE tasks ADD CONSTRAINT tasks_state_check CHECK (state IN ('queued','leased','committed','disputed','failed'));
ALTER TABLE leases DROP CONSTRAINT leases_state_check;
ALTER TABLE leases ADD CONSTRAINT leases_state_check CHECK (state IN ('active','completed','expired','failed'));
CREATE TABLE attempts (
 id UUID PRIMARY KEY REFERENCES leases(attempt_id),
 task_id UUID NOT NULL REFERENCES tasks(id),
 device_id UUID NOT NULL REFERENCES devices(id),
 reason INTEGER CHECK (reason BETWEEN 1 AND 9),
 code VARCHAR(32),
 UNIQUE(task_id, device_id, id)
);
CREATE UNIQUE INDEX one_live_device_task ON leases(task_id,device_id) WHERE state = 'active';
CREATE TABLE blobs (
 sha256 TEXT PRIMARY KEY CHECK (sha256 ~ '^[0-9a-f]{64}$'),
 size BIGINT NOT NULL CHECK (size BETWEEN 0 AND 16777216),
 uploader UUID NOT NULL REFERENCES devices(id),
 created_at BIGINT NOT NULL
);
-- Every authenticated uploader retains proof of possession even when content is deduplicated.
CREATE TABLE blob_uploaders (
 sha256 TEXT NOT NULL REFERENCES blobs(sha256),
 device_id UUID NOT NULL REFERENCES devices(id),
 PRIMARY KEY(sha256,device_id)
);
CREATE TABLE job_blobs (
 job_id UUID NOT NULL REFERENCES jobs(id),
 sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
 size BIGINT NOT NULL CHECK (size BETWEEN 1 AND 16777216),
 PRIMARY KEY(job_id,sha256)
);
CREATE TABLE task_results (
 attempt_id UUID PRIMARY KEY REFERENCES attempts(id),
 task_id UUID NOT NULL REFERENCES tasks(id),
 device_id UUID NOT NULL REFERENCES devices(id),
 sha256 TEXT NOT NULL REFERENCES blobs(sha256),
 size BIGINT NOT NULL,
 UNIQUE(task_id,device_id)
);
CREATE INDEX jobs_requester ON jobs(requester);
CREATE INDEX tasks_jobs ON tasks(job_id);
CREATE INDEX job_blobs_hash ON job_blobs(sha256);
