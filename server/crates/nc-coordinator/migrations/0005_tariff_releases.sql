-- 1.L2: TARIFF_WINDOW failures are released without counting toward the attempt cap; this
-- counter bounds how many times (MAX_TARIFF_RELEASES in code).
-- 1.L4 needs no schema change: a 2-of-2 mismatch now schedules a third replica and 'disputed'
-- means no 2-of-3 majority. Rows already 'disputed' stay terminal (no backfill).
ALTER TABLE tasks ADD COLUMN tariff_releases INTEGER NOT NULL DEFAULT 0 CHECK (tariff_releases >= 0);
