"""SQLite persistence for workers, jobs, leases and attempts.

All state lives on disk, so the controller can restart without losing queued work.
A single connection guarded by a lock keeps claiming atomic (no job is handed out twice).
"""
from __future__ import annotations

import json
import secrets
import sqlite3
import threading
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator

SCHEMA = """
create table if not exists workers(
  id text primary key, name text unique not null, token_hash text not null default '',
  state text not null default 'active', created real not null, last_seen real,
  info text not null default '{}', metrics text not null default '{}', version text not null default '');
create table if not exists batches(
  id text primary key, label text, created real not null, idem_key text unique, total integer not null,
  kind text not null default '');
create table if not exists jobs(
  id text primary key, batch_id text not null, idx integer not null, kind text not null, params text not null,
  status text not null, progress real not null default 0, result text, error text,
  attempt integer not null default 0, max_attempts integer not null, timeout_s integer not null, target text,
  worker_id text, worker_name text, created real not null, started real, finished real, lease_expires real,
  cancel_requested integer not null default 0, duration real);
create index if not exists jobs_status on jobs(status, created);
create index if not exists jobs_batch on jobs(batch_id, idx);
create table if not exists attempts(
  job_id text, attempt integer, worker_id text, worker_name text, started real, ended real, outcome text, error text);
"""

TERMINAL = ("succeeded", "failed", "cancelled")


class Store:
    def __init__(self, path: str | Path) -> None:
        existed = Path(path).is_file()
        self._conn = sqlite3.connect(str(path), check_same_thread=False, isolation_level=None)
        self._conn.row_factory = sqlite3.Row
        self._lock = threading.RLock()
        with self._lock:
            self._conn.execute("pragma journal_mode=WAL")
            self._conn.execute("pragma synchronous=NORMAL")
            self._conn.executescript(SCHEMA)
        self._migrate(Path(path), existed)
        self._last_prune = 0.0
        self.prune_samples()

    def _migrate(self, path: Path, existed: bool) -> None:
        self._conn.execute("create table if not exists meta(key text primary key, value text not null)")
        row = self._conn.execute("select value from meta where key='schema_version'").fetchone()
        version = int(row[0]) if row else 0
        if version >= 1:
            return
        if existed:
            backup = path.with_name(path.name + ".bak-0.1.0")
            if not backup.exists():
                with sqlite3.connect(str(backup)) as dest:
                    self._conn.backup(dest)
        with self._tx() as c:
            for table, columns in {
                "jobs": {"cpu_seconds": "real", "peak_mem_mb": "real", "queue_wait_s": "real", "link": "text", "eligible_workers": "text"},
                "attempts": {"duration": "real", "cpu_seconds": "real", "peak_mem_mb": "real"},
            }.items():
                existing = {r[1] for r in c.execute(f"pragma table_info({table})")}
                for name, kind in columns.items():
                    if name not in existing:
                        c.execute(f"alter table {table} add column {name} {kind}")
            c.execute("create table if not exists worker_samples(worker_id text, ts real, cpu_percent real, mem_percent real, running_jobs integer, rtt_ms real)")
            c.execute("create index if not exists samples_worker_ts on worker_samples(worker_id,ts)")
            c.execute("create table if not exists benchmarks(id text primary key, created real, label text, spec text, status text, result text, error text, idem_key text unique)")
            c.execute("create table if not exists benchmark_runs(benchmark_id text, scenario text, repeat integer, batch_id text, started real, finished real, wall_s real, settings text, per_worker text, answer text, valid integer)")
            c.execute("insert or replace into meta values('schema_version','1')")

    def prune_samples(self) -> None:
        now = time.time()
        with self._tx() as c:
            c.execute("delete from worker_samples where ts<?", (now - 86400,))
        self._last_prune = now

    def samples(self, worker_id: str, t0: float, t1: float) -> list[dict[str, Any]]:
        return [dict(r) for r in self._q("select * from worker_samples where worker_id=? and ts between ? and ? order by ts", (worker_id, t0, t1))]

    def close(self) -> None:
        with self._lock:
            self._conn.close()

    @contextmanager
    def _tx(self) -> Iterator[sqlite3.Connection]:
        with self._lock:
            self._conn.execute("begin immediate")
            try:
                yield self._conn
            except BaseException:
                self._conn.execute("rollback")
                raise
            self._conn.execute("commit")

    def _q(self, sql: str, args: tuple = ()) -> list[sqlite3.Row]:
        with self._lock:
            return self._conn.execute(sql, args).fetchall()

    # ------------------------------------------------------------ workers
    def add_worker(self, name: str, token_hash: str, info: dict[str, Any]) -> str:
        """Create a worker, or rotate the token of an existing one (admin authorised via pairing code)."""
        with self._tx() as c:
            row = c.execute("select id from workers where name=?", (name,)).fetchone()
            if row:
                c.execute(
                    "update workers set token_hash=?, state='active', info=?, last_seen=? where id=?",
                    (token_hash, json.dumps(info), time.time(), row["id"]),
                )
                return str(row["id"])
            wid = uuid.uuid4().hex[:12]
            c.execute(
                "insert into workers(id,name,token_hash,state,created,last_seen,info) values(?,?,?,?,?,?,?)",
                (wid, name, token_hash, "active", time.time(), time.time(), json.dumps(info)),
            )
            return wid

    def worker_by_token(self, token_hash: str) -> dict[str, Any] | None:
        if not token_hash:
            return None
        rows = self._q("select * from workers where token_hash=? and state!='revoked'", (token_hash,))
        return dict(rows[0]) if rows else None

    def get_worker(self, ident: str) -> dict[str, Any] | None:
        rows = self._q("select * from workers where id=? or name=?", (ident, ident))
        return dict(rows[0]) if rows else None

    def list_workers(self) -> list[dict[str, Any]]:
        workers = [dict(r) for r in self._q("select * from workers order by created")]
        stats: dict[tuple[str, str], int] = {}
        for r in self._q("select worker_id, status, count(*) n from jobs where worker_id is not null group by 1,2"):
            stats[(r["worker_id"], r["status"])] = r["n"]
        for w in workers:
            w["info"] = json.loads(w["info"])
            w["metrics"] = json.loads(w["metrics"])
            w["link"] = w["metrics"].get("link", "unknown")
            w["running"] = stats.get((w["id"], "running"), 0)
            w["succeeded"] = stats.get((w["id"], "succeeded"), 0)
            w["failed"] = stats.get((w["id"], "failed"), 0)
            w.pop("token_hash", None)
        return workers

    def set_worker_state(self, ident: str, state: str) -> bool:
        with self._tx() as c:
            w = c.execute("select id from workers where id=? or name=?", (ident, ident)).fetchone()
            if not w:
                return False
            if state == "revoked":
                c.execute("update workers set state='revoked', token_hash='' where id=?", (w["id"],))
                self._requeue_running(c, w["id"], "worker revoked")
            else:
                c.execute("update workers set state=? where id=?", (state, w["id"]))
            return True

    def touch_worker(self, wid: str, info: dict[str, Any] | None, metrics: dict[str, Any], version: str) -> None:
        with self._tx() as c:
            c.execute("insert into worker_samples values(?,?,?,?,?,?)", (wid, time.time(), metrics.get("cpu_percent"), metrics.get("mem_percent"), metrics.get("running_jobs", 0), metrics.get("rtt_ms")))
            if info:
                c.execute(
                    "update workers set last_seen=?, metrics=?, version=?, info=? where id=?",
                    (time.time(), json.dumps(metrics), version, json.dumps(info), wid),
                )
            else:
                c.execute(
                    "update workers set last_seen=?, metrics=?, version=? where id=?",
                    (time.time(), json.dumps(metrics), version, wid),
                )
        if time.time() - self._last_prune > 3600:
            self.prune_samples()

    # ------------------------------------------------------------ submitting
    def create_batch(
        self, kind: str, label: str | None, idem_key: str | None, jobs: list[tuple[str, dict[str, Any]]],
        timeout_s: int, max_attempts: int, target: str | None,
        eligible_workers: list[str] | None = None,
    ) -> tuple[str, bool]:
        """Insert a batch of jobs. A repeated idempotency key returns the original batch (duplicate prevention)."""
        with self._tx() as c:
            if idem_key:
                row = c.execute("select id from batches where idem_key=?", (idem_key,)).fetchone()
                if row:
                    return str(row["id"]), False
            bid = uuid.uuid4().hex[:12]
            now = time.time()
            c.execute(
                "insert into batches(id,label,created,idem_key,total,kind) values(?,?,?,?,?,?)",
                (bid, label, now, idem_key, len(jobs), kind),
            )
            for idx, (job_kind, params) in enumerate(jobs):
                c.execute(
                    "insert into jobs(id,batch_id,idx,kind,params,status,max_attempts,timeout_s,target,created,eligible_workers)"
                    " values(?,?,?,?,?,?,?,?,?,?,?)",
                    (secrets.token_hex(6), bid, idx, job_kind, json.dumps(params), "queued",
                     max_attempts, timeout_s, target, now, json.dumps(eligible_workers) if eligible_workers else None),
                )
            return bid, True

    # ------------------------------------------------------------ worker protocol
    def claim(self, worker: dict[str, Any], limit: int, lease_s: int) -> list[dict[str, Any]]:
        now = time.time()
        with self._tx() as c:
            saved_worker = c.execute("select metrics from workers where id=?", (worker["id"],)).fetchone()
            link = json.loads(saved_worker["metrics"]).get("link", "unknown") if saved_worker else "unknown"
            if link not in {"lan", "wifi-direct", "loopback"}:
                link = "unknown"
            rows = c.execute(
                "select id from jobs where status='queued' and (target is null or target=?)"
                " and (eligible_workers is null or exists(select 1 from json_each(eligible_workers) where value=?))"
                " order by created, idx limit ?",
                (worker["name"], worker["name"], limit),
            ).fetchall()
            claimed = []
            for r in rows:
                c.execute(
                    "update jobs set status='running', worker_id=?, worker_name=?, attempt=attempt+1,"
                    " started=?, lease_expires=?, progress=0, error=coalesce(error,''), queue_wait_s=?-created, link=? where id=?",
                    (worker["id"], worker["name"], now, now + lease_s, now, link, r["id"]),
                )
                job = c.execute("select * from jobs where id=?", (r["id"],)).fetchone()
                c.execute(
                    "insert into attempts(job_id,attempt,worker_id,worker_name,started) values(?,?,?,?,?)",
                    (job["id"], job["attempt"], worker["id"], worker["name"], now),
                )
                claimed.append(self._job(job, params=True))
            return claimed

    def heartbeat_jobs(
        self, worker_id: str, reported: list[dict[str, Any]], lease_s: int
    ) -> tuple[list[str], list[str]]:
        """Extend leases for jobs the worker still owns. Returns (cancel_ids, abandon_ids)."""
        now = time.time()
        cancel, abandon = [], []
        with self._tx() as c:
            for rep in reported:
                job = c.execute("select * from jobs where id=?", (rep["id"],)).fetchone()
                if not job or job["worker_id"] != worker_id or job["attempt"] != rep["attempt"] or job["status"] != "running":
                    abandon.append(rep["id"])
                    continue
                c.execute(
                    "update jobs set lease_expires=?, progress=? where id=?",
                    (now + lease_s, max(0.0, min(100.0, float(rep["progress"]))), job["id"]),
                )
                if job["cancel_requested"]:
                    cancel.append(job["id"])
        return cancel, abandon

    def finish(
        self, job_id: str, worker_id: str, attempt: int, *, result: Any = None, error: str | None = None,
        retryable: bool = False, cancelled: bool = False, duration: float = 0.0,
        cpu_seconds: float | None = None, peak_mem_mb: float | None = None,
    ) -> str | None:
        """Record the outcome of an attempt. Returns the new status, or None if the report is stale."""
        now = time.time()
        with self._tx() as c:
            job = c.execute("select * from jobs where id=?", (job_id,)).fetchone()
            if not job or job["worker_id"] != worker_id or job["attempt"] != attempt or job["status"] in TERMINAL:
                return None
            if error is None and not cancelled and not job["cancel_requested"]:
                status, outcome = "succeeded", "succeeded"
            elif cancelled or job["cancel_requested"]:
                status, outcome = "cancelled", "cancelled"
            elif retryable and job["attempt"] < job["max_attempts"]:
                status, outcome = "queued", "retry"
            else:
                status, outcome = "failed", "failed"
            c.execute(
                "update jobs set status=?, result=?, error=?, finished=?, duration=?, lease_expires=null,"
                " progress=?, cpu_seconds=?, peak_mem_mb=? where id=?",
                (status, json.dumps(result) if status == "succeeded" else None,
                 error if status != "succeeded" else None, now if status != "queued" else None,
                 duration, 100.0 if status == "succeeded" else job["progress"], cpu_seconds, peak_mem_mb, job_id),
            )
            if status == "queued":
                c.execute("update jobs set worker_id=null, worker_name=null where id=?", (job_id,))
            c.execute(
                "update attempts set ended=?, outcome=?, error=?, duration=?, cpu_seconds=?, peak_mem_mb=? where job_id=? and attempt=? and outcome is null",
                (now, outcome, error, duration, cpu_seconds, peak_mem_mb, job_id, attempt),
            )
            return status

    def release(self, job_id: str, worker_id: str, attempt: int) -> bool:
        """A worker shutting down gracefully hands a job back without using up a retry."""
        with self._tx() as c:
            job = c.execute("select * from jobs where id=?", (job_id,)).fetchone()
            if not job or job["worker_id"] != worker_id or job["attempt"] != attempt or job["status"] != "running":
                return False
            c.execute(
                "update jobs set status='queued', worker_id=null, worker_name=null, attempt=attempt-1,"
                " lease_expires=null, progress=0 where id=?", (job_id,),
            )
            c.execute("update attempts set ended=?, duration=?-started, outcome='released' where job_id=? and attempt=? and outcome is null", (time.time(), time.time(), job_id, attempt))
            return True

    # ------------------------------------------------------------ cancel / retry / maintenance
    def cancel_all(self, statuses: tuple[str, ...] = ("queued", "running")) -> dict[str, int]:
        with self._tx() as c:
            cancelled = c.execute("update jobs set status='cancelled', finished=? where status='queued'", (time.time(),)).rowcount if "queued" in statuses else 0
            cancelling = c.execute("update jobs set cancel_requested=1 where status='running'").rowcount if "running" in statuses else 0
        return {"cancelled": cancelled, "cancelling": cancelling}
    def cancel_job(self, job_id: str) -> str | None:
        with self._tx() as c:
            job = c.execute("select * from jobs where id=?", (job_id,)).fetchone()
            if not job:
                return None
            if job["status"] == "queued":
                c.execute("update jobs set status='cancelled', finished=? where id=?", (time.time(), job_id))
                return "cancelled"
            if job["status"] == "running":
                c.execute("update jobs set cancel_requested=1 where id=?", (job_id,))
                return "cancelling"
            return job["status"]

    def cancel_batch(self, batch_id: str) -> int:
        ids = [r["id"] for r in self._q(
            "select id from jobs where batch_id=? and status in ('queued','running')", (batch_id,))]
        for jid in ids:
            self.cancel_job(jid)
        return len(ids)

    def retry_job(self, job_id: str) -> bool:
        with self._tx() as c:
            job = c.execute("select * from jobs where id=?", (job_id,)).fetchone()
            if not job or job["status"] not in ("failed", "cancelled"):
                return False
            c.execute(
                "update jobs set status='queued', max_attempts=attempt+1, error=null,"
                " cancel_requested=0, worker_id=null, worker_name=null, finished=null, progress=0 where id=?",
                (job_id,),
            )
            return True

    def _requeue_running(self, c: sqlite3.Connection, worker_id: str, reason: str) -> None:
        for job in c.execute("select * from jobs where status='running' and worker_id=?", (worker_id,)).fetchall():
            self._fail_or_requeue(c, job, reason)

    def _fail_or_requeue(self, c: sqlite3.Connection, job: sqlite3.Row, reason: str) -> None:
        now = time.time()
        if job["cancel_requested"]:
            status = "cancelled"
        elif job["attempt"] < job["max_attempts"]:
            status = "queued"
        else:
            status = "failed"
        c.execute(
            "update jobs set status=?, error=?, finished=?, lease_expires=null, worker_id=case when ?='queued' then null else worker_id end,"
            " worker_name=case when ?='queued' then null else worker_name end where id=?",
            (status, reason, now if status != "queued" else None, status, status, job["id"]),
        )
        c.execute(
            "update attempts set ended=?, duration=?-started, outcome=?, error=? where job_id=? and attempt=? and outcome is null",
            (now, now, "lost", reason, job["id"], job["attempt"]),
        )

    def expire_leases(self, hard_timeout_grace_s: int = 30) -> int:
        """Re-queue (or fail) jobs whose worker went silent or ran past its timeout."""
        now = time.time()
        count = 0
        with self._tx() as c:
            for job in c.execute("select * from jobs where status='running'").fetchall():
                if job["lease_expires"] is not None and job["lease_expires"] < now:
                    self._fail_or_requeue(c, job, f"lease expired (worker {job['worker_name']} stopped responding)")
                    count += 1
                elif job["started"] and now > job["started"] + job["timeout_s"] + hard_timeout_grace_s:
                    self._fail_or_requeue(c, job, f"timed out after {job['timeout_s']}s")
                    count += 1
        return count

    def recover_on_start(self, lease_s: int) -> int:
        """After a controller restart, give workers one lease period to reconnect and keep their jobs."""
        with self._tx() as c:
            cur = c.execute(
                "update jobs set lease_expires=? where status='running'", (time.time() + lease_s * 2,))
            return cur.rowcount

    # ------------------------------------------------------------ reading
    def _job(self, row: sqlite3.Row, params: bool = False) -> dict[str, Any]:
        d = dict(row)
        d["result"] = json.loads(d["result"]) if d["result"] else None
        d["params"] = json.loads(d["params"]) if params else None
        if not params:
            d.pop("params")
        d["cancel_requested"] = bool(d["cancel_requested"])
        return d

    def get_job(self, job_id: str) -> dict[str, Any] | None:
        rows = self._q("select * from jobs where id=?", (job_id,))
        if not rows:
            return None
        job = self._job(rows[0], params=False)
        job["attempts"] = [dict(r) for r in self._q("select * from attempts where job_id=? order by attempt", (job_id,))]
        return job

    def list_jobs(self, status: str | None = None, batch_id: str | None = None, limit: int = 300) -> list[dict[str, Any]]:
        sql, args = "select * from jobs", []
        clauses = []
        if status:
            clauses.append("status=?")
            args.append(status)
        if batch_id:
            clauses.append("batch_id=?")
            args.append(batch_id)
        if clauses:
            sql += " where " + " and ".join(clauses)
        sql += " order by created desc, idx limit ?"
        args.append(limit)
        return [self._job(r) for r in self._q(sql, tuple(args))]

    def get_batch(self, batch_id: str) -> dict[str, Any] | None:
        rows = self._q("select * from batches where id=?", (batch_id,))
        if not rows:
            return None
        batch = dict(rows[0])
        counts = {r["status"]: r["n"] for r in self._q(
            "select status, count(*) n from jobs where batch_id=? group by 1", (batch_id,))}
        batch["counts"] = counts
        batch["done"] = batch["total"] == sum(counts.get(s, 0) for s in TERMINAL)
        by_worker: dict[str, dict[str, Any]] = {}
        for r in self._q(
            "select worker_name, count(*) n, sum(duration) busy from jobs"
            " where batch_id=? and status='succeeded' group by 1", (batch_id,)):
            by_worker[r["worker_name"]] = {"chunks": r["n"], "busy_seconds": round(r["busy"] or 0, 2)}
        batch["by_worker"] = by_worker
        first_start = self._q("select min(started) s, max(finished) f from jobs where batch_id=?", (batch_id,))[0]
        if first_start["s"] and first_start["f"] and batch["done"]:
            batch["wall_seconds"] = round(first_start["f"] - first_start["s"], 2)
        return batch

    def batch_results(self, batch_id: str) -> list[dict[str, Any]]:
        return [dict(r) | {"result": json.loads(r["result"]) if r["result"] else None}
                for r in self._q("select idx, result, status from jobs where batch_id=? order by idx", (batch_id,))]

    def list_batches(self, limit: int = 50) -> list[dict[str, Any]]:
        return [self.get_batch(r["id"]) for r in self._q("select id from batches order by created desc limit ?", (limit,))]  # type: ignore[misc]

    def summary(self) -> dict[str, int]:
        counts = {r["status"]: r["n"] for r in self._q("select status, count(*) n from jobs group by 1")}
        return {s: counts.get(s, 0) for s in ("queued", "running", "succeeded", "failed", "cancelled")}

    # ------------------------------------------------------------ benchmarks
    def create_benchmark(self, spec: dict[str, Any], idem_key: str | None = None) -> tuple[str, bool]:
        with self._tx() as c:
            if idem_key:
                row = c.execute("select id from benchmarks where idem_key=?", (idem_key,)).fetchone()
                if row:
                    return row[0], False
            ident = uuid.uuid4().hex[:12]
            c.execute("insert into benchmarks(id,created,label,spec,status,idem_key) values(?,?,?,?,?,?)", (ident, time.time(), spec.get("label"), json.dumps(spec), "running", idem_key))
            return ident, True

    def update_benchmark(self, ident: str, status: str, result: dict | None = None, error: str | None = None) -> None:
        with self._tx() as c:
            c.execute("update benchmarks set status=?,result=?,error=? where id=?", (status, json.dumps(result) if result is not None else None, error, ident))

    def add_benchmark_run(self, ident: str, run: dict[str, Any]) -> None:
        with self._tx() as c:
            c.execute("insert into benchmark_runs values(?,?,?,?,?,?,?,?,?,?,?)", (ident, run["scenario"], run["repeat"], run["batch_id"], run["started"], run["finished"], run["wall_s"], json.dumps(run["settings"]), json.dumps(run["per_worker"]), json.dumps(run["answer"], sort_keys=True), int(run["valid"])))

    def benchmark(self, ident: str) -> dict[str, Any] | None:
        rows = self._q("select * from benchmarks where id=?", (ident,))
        if not rows:
            return None
        item = dict(rows[0])
        for key in ("spec", "result"):
            item[key] = json.loads(item[key]) if item[key] else None
        item.pop("idem_key", None)
        item["runs"] = []
        for row in self._q("select * from benchmark_runs where benchmark_id=? order by rowid", (ident,)):
            run = dict(row)
            for key in ("settings", "per_worker", "answer"):
                run[key] = json.loads(run[key])
            item["runs"].append(run)
        return item

    def benchmarks(self, limit: int = 50) -> list[dict[str, Any]]:
        return [self.benchmark(r["id"]) for r in self._q("select id from benchmarks order by created desc limit ?", (limit,))]

    def interrupt_benchmarks(self) -> None:
        with self._tx() as c:
            for row in c.execute("select id from benchmarks where status='running'").fetchall():
                batch_ids = c.execute("select id from batches where label like ?", (f"bench:{row['id']}:%",)).fetchall()
                for batch in batch_ids:
                    c.execute("update jobs set status='cancelled', finished=? where batch_id=? and status='queued'", (time.time(), batch["id"]))
                    c.execute("update jobs set cancel_requested=1 where batch_id=? and status='running'", (batch["id"],))
            c.execute("update benchmarks set status='interrupted',error='Controller restarted; run again for comparable measurements.' where status='running'")

    def delete_benchmark(self, ident: str) -> bool:
        with self._tx() as c:
            row = c.execute("select status from benchmarks where id=?", (ident,)).fetchone()
            if not row or row[0] == "running":
                return False
            c.execute("delete from benchmark_runs where benchmark_id=?", (ident,))
            c.execute("delete from benchmarks where id=?", (ident,))
            return True

    def latest_participant_benchmark(self, worker_name: str) -> dict[str, Any] | None:
        rows = self._q("select b.id from benchmarks b where b.status='finished' and exists(select 1 from benchmark_runs r, json_each(r.per_worker) w where r.benchmark_id=b.id and w.key=? and json_extract(w.value,'$.chunks')>0) order by b.created desc limit 1", (worker_name,))
        return self.benchmark(rows[0]["id"]) if rows else None
