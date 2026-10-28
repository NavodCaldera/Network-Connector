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
from .client import AdminClient, ControllerError, admin_client_from_disk, pair_worker
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


def _wasm_items(client: AdminClient, a: argparse.Namespace, params: dict[str, Any]) -> list[dict[str, Any]]:
    """One wasm job per input (the module is uploaded once and shared by hash)."""
    if a.module:
        up = client.upload_module(Path(a.module).read_bytes(), Path(a.module).name)
        print(f"module {up['sha256']}  ({up['size']} bytes, {'uploaded' if up['created'] else 'already uploaded'})")
        params["module_sha256"] = up["sha256"]
    if a.wasm_args:
        params["args"] = a.wasm_args
    limits = dict(params.get("limits", {}))
    for key, value in (("memory_mb", a.memory_mb), ("seconds", a.seconds), ("fuel", a.fuel)):
        if value is not None:
            limits[key] = value
    if limits:
        params["limits"] = limits
    inputs = [Path(f).read_bytes() for f in a.inputs or []]
    if a.input_text is not None:
        inputs.append(a.input_text.encode("utf-8"))
    return [{"kind": "wasm", "params": params | ({"input_b64": base64.b64encode(data).decode()} if inputs else {})}
            for data in (inputs or [None])]


def _print_wasm_outputs(client: AdminClient, batch_id: str) -> None:
    for job in sorted(client.jobs(batch_id=batch_id), key=lambda j: j["idx"]):
        result = job.get("result") or {}
        print(f"--- job {job['idx']} ({job['status']}, exit code {result.get('exit_code', '-')})")
        if "stdout_b64" in result:
            print(base64.b64decode(result["stdout_b64"]).decode("utf-8", "replace"), end="")
        elif job.get("error"):
            print(job["error"])


def cmd_job_submit(a: argparse.Namespace) -> int:
    client = _admin()
    params = _parse_params(a.param, a.params_json)
    opts = {k: v for k, v in {"label": a.label, "idempotency_key": a.key, "timeout_s": a.timeout,
                              "max_attempts": a.retries, "target": a.target}.items() if v is not None}
    if a.kind == "wasm":
        res = client.submit_batch(_wasm_items(client, a, params), **opts)
        print(f"batch {res['batch_id']}  ({res['chunks']} job(s), {'created' if res['created'] else 'already existed: duplicate ignored'})")
        if a.wait:
            _wait_batch(client, res["batch_id"])
            _print_wasm_outputs(client, res["batch_id"])
        return 0
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


def cmd_module(a: argparse.Namespace) -> int:
    client = _admin()
    if a.sub == "upload":
        path = Path(a.file)
        res = client.upload_module(path.read_bytes(), a.label or path.name)
        print(f"{res['sha256']}  {res['size']} bytes  ({'uploaded' if res['created'] else 'already uploaded'})")
    else:
        _table([[m["sha256"], m["size"], time.strftime("%Y-%m-%d %H:%M", time.localtime(m["uploaded"])), m["label"] or ""]
                for m in client.modules()], ["SHA256", "BYTES", "UPLOADED", "LABEL"])
    return 0


def cmd_sandbox(a: argparse.Namespace) -> int:
    from . import sandbox

    if a.sub == "setup":
        if a.remove:
            sandbox.remove_runtime()
            print(f"Removed the sandbox runtime and its grant: {sandbox.runtime_home()}")
            return 0
        home = sandbox.setup_runtime()
        print(f"Sandbox runtime ready in {home} (app containers may read it; nothing else was changed).")
    if a.sub in ("setup", "status"):
        ok, reason = sandbox.available(refresh=True)
        print(f"Sandbox available: {'yes' if ok else 'no'}" + ("" if ok else f"\n  {reason}"))
        print(f"Runtime folder: {sandbox.runtime_home()}\nPython: {sandbox.base_python()}")
        print(f"Leftover sandbox profiles: {len(sandbox.leftovers())}")
        if ok and getattr(a, "check", False):
            r = sandbox.self_check()
            print(f"Self-check: {r.outcome}, output {r.stdout!r}, {r.wall_s * 1000:.0f} ms, profile deleted")
        return 0 if ok else 1
    if a.sub == "sweep":
        print(f"Removed {sandbox.sweep()} leftover sandbox profile(s).")
        return 0
    data = b"".join(Path(f).read_bytes() for f in a.inputs or [])
    from . import modules

    module = Path(a.file).read_bytes()
    modules.check(module)
    if a.input_text is not None:
        data += a.input_text.encode("utf-8")
    r = sandbox.run(module, data, a.wasm_args or [], memory_mb=a.memory_mb or jobs.WASM_DEFAULT_LIMITS["memory_mb"],
                    seconds=a.seconds or jobs.WASM_DEFAULT_LIMITS["seconds"], fuel=a.fuel)
    sys.stdout.buffer.write(r.stdout)
    sys.stdout.flush()
    print(f"[{r.outcome}{' ' + r.detail if r.detail else ''}] exit code {r.exit_code}, {r.wall_s:.2f} s, "
          f"cpu {r.cpu_seconds:.2f} s, peak {r.peak_mem_mb:.0f} MB" + (f", fuel {r.fuel_used}" if r.fuel_used else ""),
          file=sys.stderr)
    return (r.exit_code or 0) if r.ok else 1


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
    wdir = _wdir(a)
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
    status_file = _wdir(a) / "status.json"
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


def _wasm_options(p: argparse.ArgumentParser) -> None:
    p.add_argument("--input", dest="inputs", action="append", metavar="FILE",
                   help="wasm: a file given to the module on stdin; repeat for one job per file")
    p.add_argument("--input-text", help="wasm: text given to the module on stdin")
    p.add_argument("--arg", dest="wasm_args", action="append", metavar="TEXT", help="wasm: one command-line argument for the module (write --arg=-x for values starting with -)")
    p.add_argument("--memory-mb", type=int, help="wasm: memory limit in MB")
    p.add_argument("--seconds", type=int, help="wasm: time limit in seconds")
    p.add_argument("--fuel", type=int, help="wasm: instruction budget (optional, about 8%% slower)")


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
    js.add_argument("kind", choices=sorted(jobs.ALL_KINDS))
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
    _wasm_options(js)
    js.add_argument("--module", help="wasm: a .wasm file to upload and run (or --param module_sha256=...)")
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

    mod = sub.add_parser("module", help="upload and list Wasm modules").add_subparsers(dest="sub", required=True)
    mu = mod.add_parser("upload", help="upload a .wasm module (at most 10 MB) and print its sha256")
    mu.add_argument("file")
    mu.add_argument("--label")
    mu.set_defaults(fn=cmd_module)
    mod.add_parser("list").set_defaults(fn=cmd_module)

    sb = sub.add_parser("sandbox", help="the Wasm sandbox on this helper").add_subparsers(dest="sub", required=True)
    ss = sb.add_parser("setup", help="create the sandbox runtime folder and its one read grant")
    ss.add_argument("--remove", action="store_true", help="remove the runtime folder and the grant again")
    ss.set_defaults(fn=cmd_sandbox)
    sst = sb.add_parser("status", help="can this computer run wasm jobs?")
    sst.add_argument("--check", action="store_true", help="also run a tiny module in a fresh sandbox")
    sst.set_defaults(fn=cmd_sandbox)
    sb.add_parser("sweep", help="remove sandbox profiles left by a crash").set_defaults(fn=cmd_sandbox)
    sr = sb.add_parser("run", help="run a .wasm file here, in the sandbox (to test your module)")
    sr.add_argument("file")
    _wasm_options(sr)
    sr.set_defaults(fn=cmd_sandbox)

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
    wr.set_defaults(fn=cmd_worker_run)
    for name in ("stop", "pause", "resume"):
        wc = wk.add_parser(name, help=f"{name} the running worker")
        wc.set_defaults(fn=cmd_worker_control, cmd=name)
    wk.add_parser("status").set_defaults(fn=cmd_worker_status)
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
