import time

import pytest

from network_connector.store import Store


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
