-- 1.P6a: at most one active lease per task across all devices. Defence in depth
-- behind the locked re-check in jobs::lease; a violation surfaces as 500, never a double lease.
UPDATE leases l SET state = 'expired'
 WHERE state = 'active'
   AND EXISTS (SELECT 1 FROM leases o
                WHERE o.task_id = l.task_id AND o.state = 'active' AND o.issued_at > l.issued_at);
CREATE UNIQUE INDEX one_active_lease_per_task ON leases (task_id) WHERE state = 'active';
