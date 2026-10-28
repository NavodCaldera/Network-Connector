"""The devices table (identity, trust, block) and its numbered migration step."""
import sqlite3

from network_connector.store import MIGRATIONS, SCHEMA, Store, _m1_measurement


def _version(store: Store) -> str:
    return store._q("select value from meta where key='schema_version'")[0][0]


def test_devices_upsert_trust_block(tmp_path):
    store = Store(tmp_path / "state.db")
    assert store.get_device("ab" * 32) is None
    store.upsert_device("ab" * 32, "i3-laptop", "PEM")
    first = store.get_device("ab" * 32)
    assert first["name"] == "i3-laptop" and not first["trusted"] and not first["blocked"] and first["score"] == 0
    store.upsert_device("ab" * 32, "renamed", "PEM")
    again = store.get_device("ab" * 32)
    assert again["name"] == "renamed" and again["first_seen"] == first["first_seen"]
    store.set_trusted("ab" * 32, True)
    store.set_blocked("cd" * 32, True)  # blocking works before the device was ever seen
    devices = {d["device_id"]: d for d in store.list_devices()}
    assert devices["ab" * 32]["trusted"] and devices["cd" * 32]["blocked"]
    store.set_trusted("ab" * 32, False)
    assert not store.get_device("ab" * 32)["trusted"]
    store.close()


def test_v0_database_migrates_to_latest_with_devices(tmp_path):
    path = tmp_path / "state.db"
    with sqlite3.connect(path) as conn:
        conn.executescript(SCHEMA)
        conn.execute("insert into workers(id,name,token_hash,created) values('old','old-worker','hash',1)")
    store = Store(path)
    assert _version(store) == str(max(MIGRATIONS))
    assert store.get_worker("old")["name"] == "old-worker"
    store.upsert_device("ef" * 32, "x", "")
    assert path.with_name("state.db.bak-0.1.0").is_file()
    store.close()


def test_v1_database_gets_step_2_and_one_backup(tmp_path):
    path = tmp_path / "state.db"
    with sqlite3.connect(path) as conn:
        conn.executescript(SCHEMA)
        _m1_measurement(conn)
        conn.execute("create table meta(key text primary key, value text not null)")
        conn.execute("insert into meta values('schema_version','1')")
        conn.execute("insert into workers(id,name,token_hash,created) values('w1','paired','hash',1)")
        conn.execute("insert into benchmarks(id,created,status) values('b1',1,'finished')")
    conn.close()
    store = Store(path)
    assert _version(store) == str(max(MIGRATIONS))
    assert store.get_worker("w1")["name"] == "paired"
    assert store._q("select count(*) from benchmarks")[0][0] == 1
    store.set_blocked("aa" * 32, True)
    backup = path.with_name("state.db.bak-v1")
    assert backup.is_file() and not path.with_name("state.db.bak-0.1.0").exists()
    conn = sqlite3.connect(backup)
    assert conn.execute("select value from meta where key='schema_version'").fetchone()[0] == "1"
    assert not conn.execute("select name from sqlite_master where name='devices'").fetchall()
    conn.close()
    store.close()
    store = Store(path)  # re-opening an up-to-date database does nothing
    assert store.get_device("aa" * 32)["blocked"]
    store.close()


def test_new_database_has_no_backup(tmp_path):
    store = Store(tmp_path / "fresh.db")
    assert _version(store) == str(max(MIGRATIONS))
    assert not list(tmp_path.glob("fresh.db.bak*"))
    store.close()
