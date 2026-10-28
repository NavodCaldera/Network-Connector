import sqlite3
import time

import pytest

from network_connector import store as store_mod
from network_connector.store import LATEST_SCHEMA, MIGRATIONS, SCHEMA, Store


@pytest.fixture
def store(tmp_path):
    s = Store(tmp_path / "t.db")
    yield s
    s.close()


def _worker(store, name="w"):
    wid = store.add_worker(name, "hash-" + name, {})
    return {"id": wid, "name": name}


def _submit(store, n=1, **kw):
    params = dict(kind="number_batch", label=None, idem_key=None, timeout_s=60, max_attempts=2, target=None)
    params.update(kw)
    return store.create_batch(jobs=[("number_batch", {"start": 1, "count": 3})] * n, **params)


def test_duplicate_submission_is_ignored(store):
    b1, created1 = _submit(store, idem_key="same")
    b2, created2 = _submit(store, idem_key="same")
    assert b1 == b2 and created1 and not created2
    assert store.summary()["queued"] == 1


def test_claim_never_hands_out_a_job_twice(store):
    _submit(store, n=5)
    w1, w2 = _worker(store, "a"), _worker(store, "b")
    got1 = store.claim(w1, 3, 60)
    got2 = store.claim(w2, 3, 60)
    ids = [j["id"] for j in got1 + got2]
    assert len(ids) == 5 and len(set(ids)) == 5


def test_target_restricts_who_can_claim(store):
    _submit(store, target="b")
    assert store.claim(_worker(store, "a"), 5, 60) == []
    assert len(store.claim(_worker(store, "b"), 5, 60)) == 1


def test_lease_expiry_requeues_then_fails_after_max_attempts(store):
    _submit(store, max_attempts=2)
    w = _worker(store)
    for attempt in (1, 2):
        job = store.claim(w, 1, lease_s=-1)[0]  # already-expired lease
        assert job["attempt"] == attempt
        assert store.expire_leases() == 1
    assert store.summary()["failed"] == 1
    assert store.summary()["queued"] == 0


def test_retry_then_success(store):
    _submit(store, max_attempts=3)
    w = _worker(store)
    j = store.claim(w, 1, 60)[0]
    assert store.finish(j["id"], w["id"], 1, error="boom", retryable=True) == "queued"
    j = store.claim(w, 1, 60)[0]
    assert j["attempt"] == 2
    assert store.finish(j["id"], w["id"], 2, result={"ok": 1}, duration=0.5) == "succeeded"


def test_non_retryable_failure_is_final(store):
    _submit(store, max_attempts=3)
    w = _worker(store)
    j = store.claim(w, 1, 60)[0]
    assert store.finish(j["id"], w["id"], 1, error="invalid", retryable=False) == "failed"


def test_stale_report_is_rejected(store):
    _submit(store)
    w1, w2 = _worker(store, "a"), _worker(store, "b")
    j = store.claim(w1, 1, lease_s=-1)[0]
    store.expire_leases()
    j2 = store.claim(w2, 1, 60)[0]
    assert store.finish(j["id"], w1["id"], 1, result={"x": 1}) is None  # the old worker lost the job
    assert store.finish(j2["id"], w2["id"], 2, result={"x": 1}) == "succeeded"


def test_cancel_queued_and_running(store):
    _submit(store, n=2)
    w = _worker(store)
    running = store.claim(w, 1, 60)[0]
    queued_id = store.list_jobs(status="queued")[0]["id"]
    assert store.cancel_job(queued_id) == "cancelled"
    assert store.cancel_job(running["id"]) == "cancelling"
    cancel, abandon = store.heartbeat_jobs(w["id"], [{"id": running["id"], "attempt": 1, "progress": 5}], 60)
    assert cancel == [running["id"]] and abandon == []
    assert store.finish(running["id"], w["id"], 1, error="cancelled", cancelled=True) == "cancelled"


def test_release_does_not_use_up_an_attempt(store):
    _submit(store, max_attempts=1)
    w = _worker(store)
    j = store.claim(w, 1, 60)[0]
    assert store.release(j["id"], w["id"], 1)
    again = store.claim(w, 1, 60)[0]
    assert again["attempt"] == 1


def test_timeout_is_enforced_even_if_worker_keeps_heartbeating(store):
    _submit(store, timeout_s=1, max_attempts=1)
    w = _worker(store)
    store.claim(w, 1, 600)
    time.sleep(0.1)
    assert store.expire_leases(hard_timeout_grace_s=-1) == 1
    assert store.summary()["failed"] == 1


def test_revoke_requeues_running_jobs_and_blocks_token(store):
    _submit(store)
    w = _worker(store)
    store.claim(w, 1, 60)
    assert store.worker_by_token("hash-w")
    store.set_worker_state("w", "revoked")
    assert store.worker_by_token("hash-w") is None
    assert store.summary()["queued"] == 1


def test_state_survives_reopen(tmp_path):
    s = Store(tmp_path / "x.db")
    _submit(s, n=3)
    s.close()
    s2 = Store(tmp_path / "x.db")
    assert s2.summary()["queued"] == 3
    s2.close()


# ------------------------------------------------------------ migrations (0.3.0)
def _meta(s, key):
    rows = s._q("select value from meta where key=?", (key,))
    return rows[0][0] if rows else None


def test_migration_steps_are_numbered():
    assert 1 in MIGRATIONS and 3 in MIGRATIONS  # step 2 (devices) belongs to the Link agent
    assert LATEST_SCHEMA == max(MIGRATIONS)


def test_0_1_0_database_upgrades_to_latest_and_keeps_rows(tmp_path):
    path = tmp_path / "state.db"
    with sqlite3.connect(path) as conn:
        conn.executescript(SCHEMA)
        conn.execute("insert into workers(id,name,token_hash,created) values('old','old-worker','hash',1)")
    s = Store(path)
    assert s.get_worker("old")["name"] == "old-worker"
    assert _meta(s, "schema_version") == str(LATEST_SCHEMA)
    assert _meta(s, "applied_steps") == ",".join(map(str, sorted(MIGRATIONS)))
    assert s.list_modules() == []
    assert path.with_name("state.db.bak-0.1.0").is_file()
    s.close()


def test_version_1_database_backs_up_once_and_gets_step_3(tmp_path):
    path = tmp_path / "state.db"
    s = Store(path)
    with s._tx() as c:  # turn it back into a 0.2.0 (version 1) database
        c.execute("drop table modules")
        c.execute("delete from meta where key='applied_steps'")
        c.execute("update meta set value='1' where key='schema_version'")
    s.close()
    s = Store(path)
    assert s.add_module("ab" * 32, b"\0asm", "x")
    assert _meta(s, "schema_version") == str(LATEST_SCHEMA)
    s.close()
    backup = path.with_name("state.db.bak-v1")
    assert backup.is_file()
    with sqlite3.connect(backup) as conn:
        assert conn.execute("select value from meta where key='schema_version'").fetchone()[0] == "1"
    stamp = backup.stat().st_mtime_ns
    Store(path).close()  # nothing missing: no new backup, no error
    assert backup.stat().st_mtime_ns == stamp


def test_gap_filled_later_still_runs(tmp_path, monkeypatch):
    """A database already at step 3 must still get step 2 when that step is merged in later."""
    path = tmp_path / "state.db"
    Store(path).close()
    ran = []
    monkeypatch.setitem(store_mod.MIGRATIONS, 2, lambda c: ran.append(2))
    s = Store(path)
    assert ran == [2]
    assert _meta(s, "applied_steps") == ",".join(map(str, sorted(store_mod.MIGRATIONS)))
    s.close()
    Store(path).close()
    assert ran == [2]  # applied once only


def test_claim_kind_filter(store):
    store.create_batch("mixed", None, None, [("number_batch", {"start": 1, "count": 3}), ("wasm", {"module_sha256": "ab" * 32})],
                       60, 1, None)
    w = _worker(store)
    assert store.claim(w, 5, 60, kinds=["prime_range"]) == []
    got = store.claim(w, 5, 60, kinds=["wasm"])
    assert [j["kind"] for j in got] == ["wasm"]
    assert store.worker_may_fetch_module(w["id"], "ab" * 32)
    assert not store.worker_may_fetch_module(w["id"], "cd" * 32)
    assert not store.worker_may_fetch_module("someone-else", "ab" * 32)
    assert [j["kind"] for j in store.claim(w, 5, 60)] == ["number_batch"]


def test_modules_are_stored_once(store):
    assert store.add_module("ab" * 32, b"\0asm\1\0\0\0", "first")
    assert not store.add_module("ab" * 32, b"\0asm\1\0\0\0", "again")
    assert store.module_data("ab" * 32) == b"\0asm\1\0\0\0"
    assert store.module_data("cd" * 32) is None
    assert [m["label"] for m in store.list_modules()] == ["first"]
    assert "data" not in store.list_modules()[0]
