-- 1.L14: a device's libp2p PeerId (derived server-side from a key bound to the device key) and its
-- registered multiaddrs, refreshed by POST /v1/devices/peer. Leases carry them as peer hints while
-- peer_seen_at is fresh. Deregistration clears them.
ALTER TABLE devices ADD COLUMN peer_id TEXT UNIQUE CHECK (char_length(peer_id) BETWEEN 1 AND 128);
ALTER TABLE devices ADD COLUMN peer_addrs TEXT[] NOT NULL DEFAULT '{}' CHECK (cardinality(peer_addrs) <= 16);
ALTER TABLE devices ADD COLUMN peer_seen_at BIGINT;
