"""Command-line interface (the GUI is the main interface; this is for scripting and servers)."""
from __future__ import annotations

import argparse
import base64
import getpass
import json
import sys
import time
from pathlib import Path
from typing import Any

import httpx

from . import __version__, jobs
from .client import (
    AdminClient,
    ControllerError,
    admin_client_from_disk,
    device_action,
    join_policy,
    pair_worker,
    requester_devices,
)
from .config import ConfigError, ControllerConfig, WorkerConfig, default_worker_limits, load_config, load_dotenv, save_config, setup_logging
from .paths import controller_dir, worker_dir


def _admin() -> AdminClient:
    try:
        client = admin_client_from_disk(controller_dir())
        client.health()
        return client
    except (httpx.HTTPError, OSError, ControllerError) as exc:
        raise SystemExit(f"Cannot reach the controller ({exc}). Start it with:  nc controller start") from exc


def _table(rows: list[list[Any]], headers: list[str]) -> None:
    data = [headers] + [["" if c is None else str(c) for c in r] for r in rows]
    widths = [max(len(r[i]) for r in data) for i in range(len(headers))]
    for n, r in enumerate(data):
        print("  ".join(c.ljust(widths[i]) for i, c in enumerate(r)).rstrip())
        if n == 0:
            print("  ".join("-" * w for w in widths))


def _parse_params(items: list[str] | None, raw_json: str | None) -> dict[str, Any]:
    params: dict[str, Any] = json.loads(raw_json) if raw_json else {}
    for item in items or []:
        key, sep, value = item.partition("=")
        if not sep:
            raise SystemExit(f"--param must look like key=value, got {item!r}")
        try:
            params[key] = json.loads(value)
        except ValueError:
            params[key] = value
    return params


# ------------------------------------------------------------------ controller
def cmd_controller_start(a: argparse.Namespace) -> int:
    from .controller import ControllerRuntime

    setup_logging("controller")
    cfg = load_config(ControllerConfig, controller_dir() / "controller.json")
    if a.host:
        if a.host != cfg.bind_host:
            cfg.bind_hosts = [h for h in cfg.bind_hosts if h != cfg.bind_host]
        cfg.bind_host = a.host
    if a.port:
        cfg.port = a.port
    try:
        rt = ControllerRuntime(cfg)
    except ConfigError as exc:
        raise SystemExit(f"error: {exc}") from exc
    rt.start()
    print(f"Controller running at https://{rt.cfg.bind_host}:{rt.cfg.port}  (Ctrl+C to stop)")
    print(f"Certificate fingerprint: {rt.fingerprint}")
    rt.wait()
    return 0


def cmd_controller_shutdown(_: argparse.Namespace) -> int:
    print(_admin().shutdown()["status"])
    return 0


def cmd_pair_code(_: argparse.Namespace) -> int:
    client = _admin()
    res = client.create_pairing()
    cfg_host = client._http.base_url.host
    print(f"\n  Pairing code:  {res['code']}\n")
    print(f"  Valid for {res['expires_in_seconds'] // 60} minutes, works once.")
    print(f"  On the worker laptop run:\n    nc worker pair --controller {cfg_host}:{client._http.base_url.port} --code {res['code']} --name <worker-name>")
    return 0


def cmd_status(a: argparse.Namespace) -> int:
    client = _admin()
    while True:
        s, workers = client.summary(), client.workers()
        if a.watch:
            print("\033[2J\033[H", end="")
        print(f"Network Connector {s['version']}   workers online {s['workers_online']}/{s['workers_total']}")
        j = s["jobs"]
        print(f"jobs: queued {j['queued']}  running {j['running']}  done {j['succeeded']}  failed {j['failed']}  cancelled {j['cancelled']}\n")
        _table([[w["name"], w["status"], f"{w['metrics'].get('cpu_percent', '-')}%", f"{w['metrics'].get('mem_percent', '-')}%",
                 w["running"], w["succeeded"], w["failed"],
                 "-" if w["seconds_since_heartbeat"] is None else f"{w['seconds_since_heartbeat']:.0f}s ago", w["version"]]
                for w in workers], ["WORKER", "STATUS", "CPU", "RAM", "RUN", "DONE", "FAIL", "HEARTBEAT", "VER"])
        if not a.watch:
            return 0
        time.sleep(2)


# ------------------------------------------------------------------ workers (admin side)
def cmd_workers(a: argparse.Namespace) -> int:
    client = _admin()
    if a.action == "list":
        return cmd_status(argparse.Namespace(watch=False))
    print(client.worker_action(a.name, a.action))
    return 0


# ------------------------------------------------------------------ jobs
def _wait_batch(client: AdminClient, batch_id: str) -> None:
    while True:
        b = client.batch(batch_id)
        print(f"  {b['counts']}  ", end="\r")
        if b["done"]:
            print()
            print(json.dumps({k: b.get(k) for k in ("combined", "by_worker", "wall_seconds", "counts")}, indent=2))
            return
        time.sleep(1)


def cmd_job_submit(a: argparse.Namespace) -> int:
    client = _admin()
    params = _parse_params(a.param, a.params_json)
    opts = {k: v for k, v in {"label": a.label, "idempotency_key": a.key, "timeout_s": a.timeout,
                              "max_attempts": a.retries, "target": a.target}.items() if v is not None}
    if a.kind in ("hash_files", "image_resize") and a.files:
        items = []
        for f in a.files:
            path = Path(f)  # the controller user explicitly chose these files
            data = base64.b64encode(path.read_bytes()).decode()
            p = dict(params)
            if a.kind == "hash_files":
                p |= {"files": [{"name": path.name, "data_b64": data}]}
            else:
                p |= {"name": path.name, "image_b64": data}
            items.append({"kind": a.kind, "params": p})
        res = client.submit_batch(items, **opts)
    else:
        res = client.submit(a.kind, params, chunks=a.chunks, **opts)
    print(f"batch {res['batch_id']}  ({res['chunks']} job(s), {'created' if res['created'] else 'already existed: duplicate ignored'})")
    if a.wait:
        _wait_batch(client, res["batch_id"])
    return 0


def cmd_job_list(a: argparse.Namespace) -> int:
    rows = [[j["id"], j["kind"], j["status"], f"{j['progress']:.0f}%", j["worker_name"] or "-", j["attempt"],
             "-" if j["duration"] is None else f"{j['duration']:.1f}s", (j["error"] or "")[:40]]
            for j in _admin().jobs(status=a.status, limit=a.limit)]
    _table(rows, ["ID", "KIND", "STATUS", "PROG", "WORKER", "TRY", "TIME", "ERROR"])
    return 0


def cmd_job_status(a: argparse.Namespace) -> int:
    client = _admin()
    try:
        print(json.dumps(client.job(a.id), indent=2, default=str))
    except ControllerError:
        print(json.dumps(client.batch(a.id), indent=2, default=str))
    return 0


def cmd_job_cancel(a: argparse.Namespace) -> int:
    client = _admin()
    try:
        print(client.cancel_job(a.id))
    except ControllerError:
        print(client.cancel_batch(a.id))
    return 0


def cmd_job_retry(a: argparse.Namespace) -> int:
    print(_admin().retry_job(a.id))
    return 0


def cmd_job_cancel_all(a: argparse.Namespace) -> int:
    if not a.yes and input("Cancel all queued and running jobs? [y/N] ").strip().lower() != "y":
        return 0
    print(json.dumps(_admin().cancel_all()))
    return 0


def cmd_bench(a: argparse.Namespace) -> int:
    from .benchmark import export_benchmark

    client = _admin()
    if a.sub == "run":
        spec = {"workload": a.workload, "size": a.size, "scenarios": a.scenarios.split(","), "repeats": a.repeats, "order": a.order, "label": a.label}
        if a.units is not None:
            spec["units"] = a.units
        ident = client.start_benchmark(spec)["id"]
        print(f"Benchmark {ident}")
        if not a.wait:
            return 0
        while True:
            item = client.benchmark(ident)
            if item["status"] != "running":
                print(json.dumps(item, indent=2))
                return 0 if item["status"] == "finished" else 1
            time.sleep(1)
    if a.sub == "list":
        _table([[b["id"], b["status"], b["label"], b["spec"]["workload"]] for b in client.benchmarks()], ["ID", "STATUS", "LABEL", "WORKLOAD"])
    elif a.sub == "cancel":
        print(json.dumps(client.cancel_benchmark(a.id)))
    elif a.sub == "show":
        item = client.benchmark(a.id)
        print(f"Benchmark {a.id}: {item['status']}")
        rows = (item.get("result") or {}).get("scenarios", [])
        _table([[r["scenario"], f"{r['wall_s']:.3f}", f"{r['min_s']:.3f}-{r['max_s']:.3f}", f"{r['units_per_s']:.3f}", r["speedup"]] for r in rows], ["SCENARIO", "WALL MEDIAN (s)", "RANGE (s)", "UNITS/S", "SPEEDUP"])
        print(json.dumps(item.get("result") or {"error": item.get("error")}, indent=2))
    elif a.sub == "export":
        content = export_benchmark(client.benchmark(a.id), a.format)
        if a.output:
            Path(a.output).write_text(content, encoding="utf-8")
            print(f"Saved {a.output}")
        else:
            print(content)
    return 0


def cmd_worker_perf(a: argparse.Namespace) -> int:
    from .client import latest_benchmark
    from .worker import load_worker_config

    wdir = _wdir(a)
    result = latest_benchmark(load_worker_config(wdir), wdir)
    print(json.dumps(result, indent=2) if result else "No completed benchmark includes this worker yet.")
    return 0


def cmd_wifidirect(a: argparse.Namespace) -> int:
    client = _admin()
    if a.sub == "start":
        result = client.direct_start(a.ssid, a.passphrase, not a.no_listen)
    elif a.sub == "stop":
        result = client.direct_stop()
    else:
        result = client.direct_status()
    # Display credentials only when starting a group, never in routine status output.
    if a.sub != "start":
        result = {k: v for k, v in result.items() if k != "passphrase"}
    print(json.dumps(result, indent=2))
    return 0


def cmd_worker_join(a: argparse.Namespace) -> int:
    from dataclasses import asdict
    from .wifidirect import join

    password = a.passphrase or getpass.getpass("Wi-Fi passphrase: ")
    info = join(a.ssid, password)
    print(json.dumps(asdict(info), indent=2))
    return 0


def cmd_worker_find(a: argparse.Namespace) -> int:
    from .link_gui import discover_controller

    directory = _wdir(a)
    cfg = load_config(WorkerConfig, directory / "worker.json")
    result = discover_controller(cfg, directory)
    if cfg.token:
        cfg.controller_host = result["host"]
        cfg.controller_fingerprint = result["fingerprint"]
        cfg.known_hosts = list(dict.fromkeys([result["host"]] + cfg.known_hosts))[:4]
        save_config(cfg, directory / "worker.json")
    print(f"Controller: {result['host']}:{cfg.controller_port}\nCertificate: {result['fingerprint']}")
    if not cfg.token:
        print("Compare the certificate fingerprint on the controller, then pair with its one-time code.")
    return 0


# ------------------------------------------------------------------ identity, nearby, trust/block, join (0.3.0)
def _local_admin() -> AdminClient | None:
    """The controller on this PC if it is running, without failing when it isn't."""
    try:
        client = admin_client_from_disk(controller_dir())
        client.health()
        return client
    except (httpx.HTTPError, OSError, ControllerError):
        return None


def cmd_identity(a: argparse.Namespace) -> int:
    from . import identity

    ident = identity.set_name(a.name) if a.name else identity.current()
    print(f"Name:       {ident.name}\nShort ID:   {ident.short_id}\nDevice ID:  {ident.device_id}\n"
          f"Folder:     {ident.cert_path.parent}")
    return 0


def cmd_nearby(a: argparse.Namespace) -> int:
    from . import identity
    from .discovery import Listener, NearbyList, probe_requesters

    ident, book = identity.current(), identity.PeerBook()
    nearby = NearbyList()
    listener = Listener(ident.device_id, nearby, port=a.port)
    listener.start()
    try:
        print(f"Listening for {a.seconds:g} s (receive-only) …")
        for entry in probe_requesters(a.probe or None, a.controller_port, own_device_id=ident.device_id):
            nearby.update(entry)
        time.sleep(a.seconds)
        status = listener.status()
    finally:
        listener.stop()
    rows = []
    for e in nearby.entries():
        badge = "blocked" if book.is_blocked(e.device_id) else "trusted" if book.is_trusted(e.device_id) else ""
        rows.append([e.name, e.short_id, badge, "open" if e.open else "closed", e.helpers, e.threads, e.link,
                     f"{e.host}:{e.port}", e.job_label])
    _table(rows, ["NAME", "ID", "BADGE", "GROUP", "HELPERS", "NEEDS", "LINK", "ADDRESS", "LABEL"])
    if not rows:
        print("No requesters found.")
    print(status["hint"] + ("" if rows else " Discovery blocked? Use: nc worker find"))
    return 0


def _set_flag(a: argparse.Namespace, flag: str) -> int:
    from . import identity

    book = identity.PeerBook()
    try:
        device_id = book.resolve(a.id)
    except ValueError:
        device_id = ""
    admin = _local_admin()
    if not device_id and admin is None:
        raise SystemExit("Unknown short ID. Use the full device ID (nc nearby shows it).")
    value = not a.undo
    if device_id:
        book.set(device_id, **{flag: value})
        print(f"This computer: {identity.short_id(device_id)} {'is' if value else 'is no longer'} {flag}.")
    if admin is not None:
        action = {"trusted": "trust", "blocked": "block"}[flag]
        result = device_action(admin, device_id or identity.normalize_device_ref(a.id), action if value else "un" + action)
        print(f"Requester on this computer: {result['action']} {identity.short_id(result['device_id'])}.")
    return 0


def cmd_trust(a: argparse.Namespace) -> int:
    return _set_flag(a, "trusted")


def cmd_block(a: argparse.Namespace) -> int:
    return _set_flag(a, "blocked")


def cmd_policy(a: argparse.Namespace) -> int:
    print(json.dumps(join_policy(_admin(), join_policy=a.policy, announce=a.announce, announce_label=a.label), indent=2))
    return 0


def cmd_devices(a: argparse.Namespace) -> int:
    from . import identity

    client = _admin()
    if a.action != "list":
        if not a.id:
            raise SystemExit(f"nc devices {a.action} needs a device ID.")
        print(json.dumps(device_action(client, identity.normalize_device_ref(a.id), a.action)))
        return 0
    data = requester_devices(client)
    _table([[d["name"], d["short_id"], "yes" if d["trusted"] else "", "yes" if d["blocked"] else "", ", ".join(d["workers"])]
            for d in data["devices"]], ["NAME", "ID", "TRUSTED", "BLOCKED", "WORKERS"])
    for r in data["requests"]:
        print(f"Waiting to join: {r['name']} ({r['short_id']}). Accept with: nc devices accept {r['short_id']}")
    for v in data["verifications"]:
        print(f"Verify {v['worker']} ({v['short_id']}): {v['code'][:3]} {v['code'][3:]}. If the helper shows the same number: nc trust {v['short_id']}")
    return 0


def _requester_dir(a: argparse.Namespace) -> Path:
    from . import identity
    from .client import joined_dir

    ref = getattr(a, "requester", None)
    if not ref:
        return _wdir(a)
    ref = identity.normalize_device_ref(ref)
    if len(ref) == 64:
        return joined_dir(ref)
    matches = sorted(p for p in worker_dir("requesters").iterdir() if p.is_dir() and p.name.startswith(ref))
    if len(matches) != 1:
        raise SystemExit("Unknown requester. Use the ID shown by nc worker connect.")
    return matches[0]


def cmd_worker_connect(a: argparse.Namespace) -> int:
    from . import identity
    from .client import JoinPending, join_requester, requester_hello

    host, _, port_text = a.to.partition(":")
    port = int(port_text or 8765)
    expected = identity.normalize_device_ref(a.device_id) if a.device_id else ""
    if len(expected) != 64:
        info = requester_hello(host, port)
        if not info or (expected and not info["device_id"].startswith(expected)):
            raise SystemExit("No open requester with that ID answered at this address. It may be off, closed, or "
                             "require a pairing code (then use: nc worker pair).")
        print(f"Requester: {info['name']}  ID {identity.short_id(info['device_id'])}  ({info['device_id']})")
        if not a.yes and input("Join it? [y/N] ").strip().lower() != "y":
            return 1
        expected = info["device_id"]
    cfg = load_config(WorkerConfig, worker_dir("worker") / "worker.json")
    for key, value in default_worker_limits().items():
        setattr(cfg, key, value)
    cfg.token = cfg.worker_id = ""
    try:
        cfg = join_requester(host, port, expected, identity.current(), None, cfg, wait_s=a.wait)
    except JoinPending as exc:
        raise SystemExit(f"{exc.message} (increase --wait to keep waiting)") from exc
    except (ControllerError, httpx.HTTPError, OSError) as exc:
        raise SystemExit(f"Join failed: {exc}") from exc
    short = identity.short_id(cfg.requester_device_id)
    print(f"Joined {cfg.requester_name} ({short}) as '{cfg.name}'. Start helping with:  nc worker run --requester {short}")
    return 0


def cmd_worker_leave(a: argparse.Namespace) -> int:
    from .client import leave_requester
    from .worker import load_worker_config

    wdir = _requester_dir(a)
    leave_requester(load_worker_config(wdir), wdir)
    print("Left the requester's group. Its token was dropped on both sides.")
    return 0


def cmd_worker_verify(a: argparse.Namespace) -> int:
    from . import identity
    from .client import verify_requester
    from .worker import load_worker_config

    wdir = _requester_dir(a)
    cfg = load_worker_config(wdir)
    code = verify_requester(cfg, wdir, identity.current())
    print(f"\n  {code[:3]} {code[3:]}\n\nThe requester's Devices view must show the same number for this computer.")
    if input("Do the numbers match? Trust this requester [y/N] ").strip().lower() == "y":
        identity.PeerBook().set(cfg.controller_fingerprint, trusted=True, name=cfg.requester_name or None)
        print("Trusted.")
    return 0


def cmd_demo(a: argparse.Namespace) -> int:
    from .demo import run_demo

    try:
        result = run_demo(_admin(), chunks=a.chunks, target=a.target)
    except RuntimeError as exc:
        raise SystemExit(f"demo: {exc}") from exc
    return 0 if result["ok"] else 1


# ------------------------------------------------------------------ worker (local agent)
def _wdir(a: argparse.Namespace) -> Path:
    return worker_dir(getattr(a, "dir", None) or "worker")


def cmd_worker_pair(a: argparse.Namespace) -> int:
    host, _, port = a.controller.partition(":")
    wdir = _wdir(a)
    cfg = load_config(WorkerConfig, wdir / "worker.json")
    for key, value in default_worker_limits().items():
        setattr(cfg, key, value)
    code = a.code or input("Pairing code: ").strip()
    name = a.name or input("Name for this worker (e.g. i3-laptop): ").strip()
    try:
        pair_worker(host, int(port or 8765), code, name, wdir, cfg)
    except (ControllerError, httpx.HTTPError, OSError) as exc:
        raise SystemExit(f"Pairing failed: {exc}") from exc
    print(f"Paired as '{name}'. Settings saved to {wdir}. Start it with:  nc worker run")
    return 0


def cmd_worker_run(a: argparse.Namespace) -> int:
    from .worker import NotPaired, WorkerAgent, load_worker_config

    setup_logging("worker")
    wdir = _requester_dir(a)
    try:
        cfg = load_worker_config(wdir)
    except NotPaired as exc:
        raise SystemExit(str(exc)) from exc
    for attr in ("max_cpu_threads", "max_concurrent_jobs", "max_memory_mb", "idle_timeout_minutes"):
        if getattr(a, attr, None) is not None:
            setattr(cfg, attr, getattr(a, attr))
    agent = WorkerAgent(cfg, wdir)
    print(f"Worker '{cfg.name}' running (Ctrl+C to stop). Limits: {agent.slots} job(s) at once, {cfg.max_memory_mb} MB each.")
    agent.start_thread()
    try:
        while agent._thread and agent._thread.is_alive():
            time.sleep(0.5)
    except KeyboardInterrupt:
        print("Stopping gracefully ...")
        agent.stop()
    return 0


def cmd_worker_control(a: argparse.Namespace) -> int:
    wdir = _wdir(a)
    (wdir / "control.txt").write_text(a.cmd + "\n", encoding="utf-8")
    print(f"Sent '{a.cmd}' to the worker (it applies within a second or two).")
    return 0


def cmd_worker_status(a: argparse.Namespace) -> int:
    status_file = _requester_dir(a) / "status.json"
    if not status_file.is_file():
        print("No status yet: the worker has not been started.")
        return 1
    s = json.loads(status_file.read_text(encoding="utf-8"))
    age = time.time() - s["updated"]
    print(f"{s['name']}  state={s['state']}  (updated {age:.0f}s ago)  running={len(s['running'])}  done={s['completed']}  failed={s['failed']}  version={s['version']}")
    return 0


def cmd_worker_update(a: argparse.Namespace) -> int:
    from . import updater

    try:
        if a.check:
            st = updater.check()
            print(f"installed {st.current}; latest release {st.latest or 'none'}; update available: {st.update_available}")
            return 0
        updater.update(verify_signature=a.verify_signature)
    except updater.UpdateError as exc:
        raise SystemExit(f"Update failed: {exc}") from exc
    return 0


# ------------------------------------------------------------------ misc
def cmd_selftest(_: argparse.Namespace) -> int:
    assert jobs.execute("number_batch", {"start": 1, "count": 4})["sum_of_squares"] == 30
    assert jobs.execute("prime_range", {"start": 0, "end": 100})["primes"] == 25
    try:
        jobs.execute("shell", {"cmd": "dir"})
    except jobs.JobError:
        pass
    else:
        raise AssertionError("allowlist is not enforced")
    import fastapi, cryptography, psutil, PIL  # noqa: F401, E401 - dependency check

    print(f"selftest ok, version {__version__}")
    return 0


def cmd_version(_: argparse.Namespace) -> int:
    print(__version__)
    return 0


def cmd_gui(_: argparse.Namespace) -> int:
    from .gui import main as run

    run()
    return 0


def cmd_worker_gui(_: argparse.Namespace) -> int:
    from .worker_gui import main as run

    run()
    return 0


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="nc", description="Network Connector: distribute safe jobs across your LAN computers.")
    p.add_argument("--version", action="version", version=__version__)
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("version", help="show the installed version").set_defaults(fn=cmd_version)
    sub.add_parser("selftest", help="quick check that the install works").set_defaults(fn=cmd_selftest)
    sub.add_parser("gui", help="open the controller window (i7)").set_defaults(fn=cmd_gui)
    sub.add_parser("worker-gui", help="open the worker window (i3)").set_defaults(fn=cmd_worker_gui)

    c = sub.add_parser("controller", help="run or stop the controller").add_subparsers(dest="sub", required=True)
    s = c.add_parser("start", help="start the controller (foreground)")
    s.add_argument("--host", help="private LAN IP to bind (never 0.0.0.0)")
    s.add_argument("--port", type=int)
    s.set_defaults(fn=cmd_controller_start)
    c.add_parser("shutdown", help="ask the running controller to stop safely").set_defaults(fn=cmd_controller_shutdown)

    sub.add_parser("pair-code", help="create a one-time pairing code").set_defaults(fn=cmd_pair_code)
    st = sub.add_parser("status", help="workers and job counts")
    st.add_argument("--watch", action="store_true", help="refresh every 2 seconds")
    st.set_defaults(fn=cmd_status)

    w = sub.add_parser("workers", help="manage registered workers (controller side)")
    w.add_argument("action", choices=["list", "pause", "resume", "disable", "enable", "revoke"])
    w.add_argument("name", nargs="?", default="")
    w.set_defaults(fn=cmd_workers)

    j = sub.add_parser("job", help="submit and manage jobs").add_subparsers(dest="sub", required=True)
    js = j.add_parser("submit", help="submit a job")
    js.add_argument("kind", choices=sorted(jobs.REGISTRY))
    js.add_argument("--param", "-p", action="append", metavar="KEY=VALUE")
    js.add_argument("--params-json")
    js.add_argument("--files", nargs="+", help="files to embed (hash_files / image_resize); one job per file")
    js.add_argument("--chunks", type=int, default=1)
    js.add_argument("--target", help="only run on this worker")
    js.add_argument("--timeout", type=int)
    js.add_argument("--retries", type=int, help="maximum attempts")
    js.add_argument("--key", help="idempotency key: submitting the same key twice creates only one job")
    js.add_argument("--label")
    js.add_argument("--wait", action="store_true")
    js.set_defaults(fn=cmd_job_submit)
    jl = j.add_parser("list")
    jl.add_argument("--status")
    jl.add_argument("--limit", type=int, default=50)
    jl.set_defaults(fn=cmd_job_list)
    ja = j.add_parser("cancel-all", help="cancel every queued and running job")
    ja.add_argument("--yes", action="store_true", help="skip the confirmation prompt")
    ja.set_defaults(fn=cmd_job_cancel_all)
    for name, fn in (("status", cmd_job_status), ("cancel", cmd_job_cancel), ("retry", cmd_job_retry)):
        jp = j.add_parser(name)
        jp.add_argument("id")
        jp.set_defaults(fn=fn)

    bench = sub.add_parser("bench", help="compare machines on identical fixed work").add_subparsers(dest="sub", required=True)
    br = bench.add_parser("run")
    br.add_argument("--workload", choices=["hash_chain", "prime_sieve"], default="hash_chain")
    size = br.add_mutually_exclusive_group()
    size.add_argument("--size", choices=["quick", "standard", "long"], default="quick")
    size.add_argument("--units", type=int)
    br.add_argument("--scenarios", default="each,all")
    br.add_argument("--repeats", type=int, default=3)
    br.add_argument("--order", choices=["fixed", "shuffled"], default="fixed")
    br.add_argument("--label", default="")
    br.add_argument("--wait", action="store_true")
    br.set_defaults(fn=cmd_bench)
    bench.add_parser("list").set_defaults(fn=cmd_bench)
    for action in ("show", "cancel", "export"):
        command = bench.add_parser(action)
        command.add_argument("id")
        command.set_defaults(fn=cmd_bench)
        if action == "export":
            command.add_argument("--format", choices=["json", "csv", "html"], default="json")
            command.add_argument("-o", "--output")

    ident = sub.add_parser("identity", help="show this computer's device identity")
    ident.add_argument("--name", help="change the friendly name shown to nearby computers")
    ident.set_defaults(fn=cmd_identity)
    near = sub.add_parser("nearby", help="list nearby requesters (receive-only listening, plus a direct-link probe)")
    near.add_argument("--seconds", type=float, default=6)
    near.add_argument("--port", type=int, default=8766, help="discovery UDP port")
    near.add_argument("--controller-port", type=int, default=8765, help="TCP port probed on direct links")
    near.add_argument("--probe", nargs="*", metavar="IP", help="also ask these addresses directly (outbound)")
    near.set_defaults(fn=cmd_nearby)
    for name, fn in (("trust", cmd_trust), ("block", cmd_block)):
        flag = sub.add_parser(name, help=f"{name} a device by its ID, here and on this computer's requester")
        flag.add_argument("id", help="device ID or short ID (AB12-CD34)")
        flag.add_argument("--undo", action="store_true", help=f"un{name} instead")
        flag.set_defaults(fn=fn)
    pol = sub.add_parser("policy", help="who may join this requester without a code")
    pol.add_argument("policy", nargs="?", choices=["anyone", "trusted", "ask", "code"])
    pol.add_argument("--announce", action=argparse.BooleanOptionalAction, default=None)
    pol.add_argument("--label", help="short text nearby helpers see (everyone nearby can read it)")
    pol.set_defaults(fn=cmd_policy)
    dev = sub.add_parser("devices", help="devices known to this requester, join requests and Verify numbers")
    dev.add_argument("action", nargs="?", default="list", choices=["list", "accept", "decline"])
    dev.add_argument("id", nargs="?")
    dev.set_defaults(fn=cmd_devices)

    d = sub.add_parser("demo", help="run the proof-of-work demo")
    d.add_argument("--chunks", type=int, default=40)
    d.add_argument("--target", help="run only on this worker (e.g. your i3) to prove it works")
    d.set_defaults(fn=cmd_demo)

    direct = sub.add_parser("wifidirect", help="manage the running controller's direct Wi-Fi group").add_subparsers(dest="sub", required=True)
    ds = direct.add_parser("start")
    ds.add_argument("--ssid", help="optional DIRECT-NC-xxxx SSID")
    ds.add_argument("--passphrase", help="optional WPA2 passphrase; omitted generates a new one")
    ds.add_argument("--no-listen", action="store_true", help="create the link without adding a controller listener")
    ds.set_defaults(fn=cmd_wifidirect)
    direct.add_parser("stop").set_defaults(fn=cmd_wifidirect)
    direct.add_parser("status").set_defaults(fn=cmd_wifidirect)

    wk = sub.add_parser("worker", help="the worker agent on this computer").add_subparsers(dest="sub", required=True)
    wp = wk.add_parser("pair", help="pair this computer with a controller")
    wp.add_argument("--controller", required=True, metavar="IP[:PORT]")
    wp.add_argument("--code")
    wp.add_argument("--name")
    wp.set_defaults(fn=cmd_worker_pair)
    wr = wk.add_parser("run", help="run the worker (foreground)")
    wr.add_argument("--max-cpu-threads", dest="max_cpu_threads", type=int)
    wr.add_argument("--max-concurrent-jobs", dest="max_concurrent_jobs", type=int)
    wr.add_argument("--max-memory-mb", dest="max_memory_mb", type=int)
    wr.add_argument("--idle-timeout-minutes", dest="idle_timeout_minutes", type=int)
    wr.add_argument("--requester", help="help a requester joined with 'nc worker connect' (its short ID)")
    wr.set_defaults(fn=cmd_worker_run)
    for name in ("stop", "pause", "resume"):
        wc = wk.add_parser(name, help=f"{name} the running worker")
        wc.set_defaults(fn=cmd_worker_control, cmd=name)
    ws = wk.add_parser("status")
    ws.add_argument("--requester")
    ws.set_defaults(fn=cmd_worker_status)
    wj = wk.add_parser("connect", help="join a nearby requester's open group (no pairing code)")
    wj.add_argument("--to", required=True, metavar="IP[:PORT]", help="the requester's address (nc nearby shows it)")
    wj.add_argument("--device-id", help="the requester's ID from nc nearby; the certificate must match it")
    wj.add_argument("--wait", type=float, default=0, help="seconds to wait if the requester asks its owner first")
    wj.add_argument("--yes", action="store_true", help="don't ask for confirmation")
    wj.set_defaults(fn=cmd_worker_connect)
    for name, fn, text in (("leave", cmd_worker_leave, "leave a joined requester's group"),
                           ("verify", cmd_worker_verify, "compare a 6-digit number with the requester, then trust it")):
        wv = wk.add_parser(name, help=text)
        wv.add_argument("--requester", help="short ID of a joined requester (default: the paired controller)")
        wv.set_defaults(fn=fn)
    wk.add_parser("perf", help="show this worker's latest shared benchmark").set_defaults(fn=cmd_worker_perf)
    join = wk.add_parser("join", help="join the controller's direct Wi-Fi or hotspot")
    join.add_argument("--ssid", required=True)
    join.add_argument("--passphrase", help="omit to enter privately")
    join.set_defaults(fn=cmd_worker_join)
    wk.add_parser("find", help="find the controller; paired workers require its pinned certificate").set_defaults(fn=cmd_worker_find)
    wu = wk.add_parser("update", help="install the newest approved release from GitHub")
    wu.add_argument("--check", action="store_true", help="only report whether an update exists")
    wu.add_argument("--verify-signature", action="store_true", help="require a GPG/SSH-signed tag")
    wu.set_defaults(fn=cmd_worker_update)
    return p


def main(argv: list[str] | None = None) -> int:
    load_dotenv()
    args = build_parser().parse_args(argv)
    try:
        return int(args.fn(args) or 0)
    except ControllerError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    except (RuntimeError, ValueError, OSError, httpx.HTTPError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
