"""Controller window for the main computer (i7): start the controller, pair workers, submit and watch jobs."""
from __future__ import annotations

import base64
import json
import time
import tkinter as tk
import uuid
from pathlib import Path
from tkinter import filedialog, messagebox, ttk
from typing import Any

from . import __version__, jobs
from .client import AdminClient, admin_client_from_disk
from .benchmark_gui import BenchmarkControls
from .link_gui import ControllerDirectControls, ControllerJoinControls
from .config import (
    ConfigError,
    ControllerConfig,
    default_worker_limits,
    list_private_ips,
    load_config,
    save_config,
    setup_logging,
)
from .gui_common import Background, Sparkline, fill_tree, make_tree
from .paths import controller_dir

GETTING_STARTED = (
    "Getting started:  1) Click 'Start controller'.  2) Optional: click 'Use this PC as a worker too' so the i7 also works.  "
    "3) Open the 'Pair a worker' tab and enter the code on the i3.  4) Try the 'Demo' tab."
)
SIMPLE_KINDS = {"number_batch", "prime_range", "monte_carlo_pi", "cpu_burn"}
FILE_KINDS = {"hash_files", "image_resize"}


def _short(value: Any) -> Any:
    """Shorten long base64 strings for display."""
    if isinstance(value, dict):
        return {k: (f"<{len(v)} chars of base64>" if k.endswith("_b64") and isinstance(v, str) else _short(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [_short(v) for v in value]
    return value


class ControllerApp(BenchmarkControls, ControllerDirectControls, ControllerJoinControls):
    def __init__(self, root: tk.Tk) -> None:
        self.root = root
        self.bg = Background(root)
        self.cfg_path = controller_dir() / "controller.json"
        self.cfg = load_config(ControllerConfig, self.cfg_path)
        self.runtime: Any = None
        self.admin: AdminClient | None = None
        self.local_agent: Any = None
        self.workers: dict[str, dict[str, Any]] = {}
        self._refreshing = False
        self._pair_expires = 0.0
        self._demo_stop = False

        root.title(f"Network Connector — Controller {__version__}")
        root.geometry("1100x720")
        root.minsize(900, 600)
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        ttk.Style().configure("Big.TLabel", font=("Segoe UI", 28, "bold"))
        ttk.Style().configure("Tile.TLabel", font=("Segoe UI", 16, "bold"))

        self._build_top()
        nb = ttk.Notebook(root)
        nb.pack(fill="both", expand=True, padx=8, pady=(0, 8))
        self._build_dashboard(nb)
        self._build_jobs(nb)
        self._build_submit(nb)
        self._build_pairing(nb)
        self._build_devices(nb)
        self._build_direct(nb)
        self._build_demo(nb)
        self._build_benchmark(nb)
        self._build_settings(nb)
        self.root.after(1000, self._tick)

    # ------------------------------------------------------------------ layout
    def _build_top(self) -> None:
        bar = ttk.Frame(self.root, padding=8)
        bar.pack(fill="x")
        self.status = ttk.Label(bar, text="Controller stopped", foreground="#b00020", font=("Segoe UI", 11, "bold"))
        self.status.pack(side="left")
        self.local_btn = ttk.Button(bar, text="Use this PC as a worker too", command=self.toggle_local_worker, state="disabled")
        self.local_btn.pack(side="right", padx=4)
        self.start_btn = ttk.Button(bar, text="Start controller", command=self.toggle_controller)
        self.start_btn.pack(side="right", padx=4)
        self.local_status = ttk.Label(bar, text="", foreground="#555")
        self.local_status.pack(side="right", padx=8)

    def _build_dashboard(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=8)
        nb.add(tab, text="Dashboard")
        ttk.Label(tab, text=GETTING_STARTED, wraplength=1050, foreground="#444").pack(fill="x", pady=(0, 8))
        tiles = ttk.Frame(tab)
        tiles.pack(fill="x")
        self.tiles: dict[str, ttk.Label] = {}
        for key, title in (("online", "Workers online"), ("queued", "Queued"), ("running", "Running"),
                           ("succeeded", "Completed"), ("failed", "Failed"), ("cancelled", "Cancelled")):
            box = ttk.LabelFrame(tiles, text=title, padding=6)
            box.pack(side="left", expand=True, fill="x", padx=4)
            self.tiles[key] = ttk.Label(box, text="–", style="Tile.TLabel")
            self.tiles[key].pack()
        ttk.Label(tab, text="Workers", font=("Segoe UI", 10, "bold")).pack(anchor="w", pady=(10, 2))
        self.wtree = make_tree(tab, [("Name", 130), ("Status", 80), ("CPU", 60), ("RAM", 60), ("CPU model", 230),
                                     ("Limits", 140), ("Running", 60), ("Done", 60), ("Failed", 60),
                                     ("Last heartbeat", 100), ("Version", 60), ("Link", 95)], height=6)
        btns = ttk.Frame(tab)
        btns.pack(fill="x", pady=4)
        for label, action in (("Pause", "pause"), ("Resume", "resume"), ("Disable", "disable"),
                              ("Enable", "enable"), ("Revoke (remove)…", "revoke")):
            ttk.Button(btns, text=label, command=lambda a=action: self.worker_action(a)).pack(side="left", padx=2)
        ttk.Label(btns, text="Select a worker, then choose an action.", foreground="#666").pack(side="left", padx=8)
        ttk.Label(tab, text="CPU usage (last few minutes)", font=("Segoe UI", 10, "bold")).pack(anchor="w", pady=(8, 2))
        self.cpu_graph = Sparkline(tab)
        self.cpu_graph.pack(fill="x")
        ttk.Label(tab, text="Memory usage", font=("Segoe UI", 10, "bold")).pack(anchor="w", pady=(8, 2))
        self.mem_graph = Sparkline(tab, height=100)
        self.mem_graph.pack(fill="x")

    def _build_jobs(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=8)
        nb.add(tab, text="Jobs")
        top = ttk.Frame(tab)
        top.pack(fill="x")
        ttk.Label(top, text="Show:").pack(side="left")
        self.job_filter = ttk.Combobox(top, values=["all", "queued", "running", "succeeded", "failed", "cancelled"],
                                       state="readonly", width=12)
        self.job_filter.set("all")
        self.job_filter.pack(side="left", padx=4)
        for label, fn in (("Cancel job", self.cancel_selected), ("Cancel whole batch", self.cancel_batch_selected),
                          ("Retry job", self.retry_selected), ("Cancel all", self.cancel_all_jobs),
                          ("Save image result…", self.save_image_result)):
            ttk.Button(top, text=label, command=fn).pack(side="left", padx=2)
        self.jtree = make_tree(tab, [("ID", 110), ("Batch", 110), ("Kind", 110), ("Status", 80), ("Progress", 70),
                                     ("Worker", 110), ("Try", 40), ("Time", 70), ("Error", 300)], height=14)
        self.jtree.bind("<<TreeviewSelect>>", lambda _e: self.show_job_detail())
        ttk.Label(tab, text="Details", font=("Segoe UI", 10, "bold")).pack(anchor="w", pady=(6, 2))
        self.job_detail = tk.Text(tab, height=10, font=("Consolas", 9))
        self.job_detail.pack(fill="both")
        self._selected_job: dict[str, Any] | None = None

    def _build_submit(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=12)
        nb.add(tab, text="Submit job")
        form = ttk.Frame(tab)
        form.pack(anchor="nw", fill="x")
        ttk.Label(form, text="Job type").grid(row=0, column=0, sticky="w", pady=3)
        self.kind = ttk.Combobox(form, values=sorted(jobs.REGISTRY), state="readonly", width=24)
        self.kind.grid(row=0, column=1, sticky="w")
        self.kind_desc = ttk.Label(form, text="", foreground="#555")
        self.kind_desc.grid(row=0, column=2, sticky="w", padx=8)
        self.kind.bind("<<ComboboxSelected>>", lambda _e: self._rebuild_params())
        self.params_frame = ttk.LabelFrame(tab, text="Parameters", padding=8)
        self.params_frame.pack(fill="x", pady=8)
        opts = ttk.LabelFrame(tab, text="Options", padding=8)
        opts.pack(fill="x")
        self.chunks = tk.IntVar(value=8)
        self.timeout = tk.IntVar(value=self.cfg.default_timeout_s)
        self.attempts = tk.IntVar(value=self.cfg.default_max_attempts)
        self.label = tk.StringVar()
        self.idem = tk.StringVar(value=uuid.uuid4().hex[:16])
        self.target = ttk.Combobox(opts, values=["(any worker)"], state="readonly", width=22)
        self.target.set("(any worker)")
        rows = [("Split into chunks", ttk.Spinbox(opts, from_=1, to=500, textvariable=self.chunks, width=8),
                 "More chunks = better sharing between computers (only for number, prime, pi and cpu jobs)."),
                ("Run on", self.target, "Pick a worker to prove it does the work, or leave as any."),
                ("Timeout (seconds)", ttk.Spinbox(opts, from_=1, to=86400, textvariable=self.timeout, width=8), ""),
                ("Max attempts", ttk.Spinbox(opts, from_=1, to=5, textvariable=self.attempts, width=8),
                 "If a worker fails or disconnects, the job is retried up to this many times."),
                ("Label", ttk.Entry(opts, textvariable=self.label, width=40), "Optional name for this batch."),
                ("Duplicate guard key", ttk.Entry(opts, textvariable=self.idem, width=40),
                 "Submitting twice with the same key creates the job only once.")]
        for r, (text, widget, hint) in enumerate(rows):
            ttk.Label(opts, text=text).grid(row=r, column=0, sticky="w", pady=3)
            widget.grid(row=r, column=1, sticky="w", padx=6)
            ttk.Label(opts, text=hint, foreground="#666").grid(row=r, column=2, sticky="w")
        ttk.Button(tab, text="Submit job", command=self.submit_job).pack(anchor="w", pady=10)
        self.submit_result = ttk.Label(tab, text="", foreground="#1b5e20")
        self.submit_result.pack(anchor="w")
        self.param_vars: dict[str, tk.Variable] = {}
        self.files: list[Path] = []
        self.kind.set("prime_range")
        self._rebuild_params()

    def _rebuild_params(self) -> None:
        for child in self.params_frame.winfo_children():
            child.destroy()
        kind = self.kind.get()
        spec = jobs.REGISTRY[kind]
        self.kind_desc.config(text=spec.description)
        self.param_vars, self.files = {}, []
        row = 0

        def add(name: str, var: tk.Variable, widget: tk.Widget | None = None, hint: str = "") -> None:
            nonlocal row
            ttk.Label(self.params_frame, text=name).grid(row=row, column=0, sticky="w", pady=2)
            (widget or ttk.Entry(self.params_frame, textvariable=var, width=20)).grid(row=row, column=1, sticky="w", padx=6)
            ttk.Label(self.params_frame, text=hint, foreground="#666").grid(row=row, column=2, sticky="w")
            self.param_vars[name] = var
            row += 1

        if kind in SIMPLE_KINDS:
            for key, value in spec.example.items():
                add(key, tk.StringVar(value=str(value)))
        elif kind == "python_function":
            fn = tk.StringVar(value="nqueens")
            box = ttk.Combobox(self.params_frame, values=sorted(jobs.FUNCTION_ARGS), textvariable=fn, state="readonly", width=18)
            add("function", fn, box, "Only these built-in functions are allowed.")
            arg = tk.StringVar(value="9")
            hint = ttk.Label(self.params_frame, foreground="#666")
            add("argument", arg)
            hint.grid(row=row, column=1, sticky="w")

            def show_range(*_: Any) -> None:
                (name, (lo, hi)), = jobs.FUNCTION_ARGS[fn.get()].items()
                hint.config(text=f"{name}: {lo} – {hi}")

            fn.trace_add("write", show_range)
            show_range()
        else:
            if kind == "hash_files":
                algo = tk.StringVar(value="sha256")
                add("algorithm", algo, ttk.Combobox(self.params_frame, values=["sha256", "sha512", "blake2b"], textvariable=algo,
                                                    state="readonly", width=12))
            else:
                add("width", tk.StringVar(value="800"))
                add("height", tk.StringVar(value="600"))
                mode, fmt = tk.StringVar(value="fit"), tk.StringVar(value="PNG")
                add("mode", mode, ttk.Combobox(self.params_frame, values=["fit", "exact"], textvariable=mode, state="readonly", width=8),
                    "fit keeps the shape; exact stretches")
                add("format", fmt, ttk.Combobox(self.params_frame, values=["PNG", "JPEG", "WEBP"], textvariable=fmt,
                                                state="readonly", width=8))
            self.files_label = ttk.Label(self.params_frame, text="No files chosen", foreground="#666")
            ttk.Button(self.params_frame, text="Choose files…", command=self.choose_files).grid(row=row, column=0, sticky="w", pady=4)
            self.files_label.grid(row=row, column=1, columnspan=2, sticky="w")
            ttk.Label(self.params_frame, text=f"Each file becomes one job (max {jobs.MAX_FILE_BYTES // 1024 // 1024} MB). "
                      "The file contents are sent with the job; workers never read your disk.",
                      foreground="#666").grid(row=row + 1, column=0, columnspan=3, sticky="w")

    def _build_pairing(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=16)
        nb.add(tab, text="Pair a worker")
        ttk.Label(tab, text="Connect a new worker (for example your i3 laptop)", font=("Segoe UI", 13, "bold")).pack(anchor="w")
        steps = ("1. Click 'Create pairing code'.\n"
                 "2. On the i3, open 'Network Connector Worker' (or run setup-worker.ps1).\n"
                 "3. Type the controller address and the code shown below, then click 'Pair'.\n"
                 "The code works once and expires after 10 minutes. 5 wrong tries lock it.")
        ttk.Label(tab, text=steps, justify="left").pack(anchor="w", pady=8)
        ttk.Button(tab, text="Create pairing code", command=self.create_code).pack(anchor="w")
        self.code_label = ttk.Label(tab, text="", style="Big.TLabel", foreground="#1a3e8c")
        self.code_label.pack(anchor="w", pady=12)
        self.code_info = ttk.Label(tab, text="", justify="left")
        self.code_info.pack(anchor="w")
        self.direct_pair_hint = ttk.Label(tab, text="", wraplength=950, justify="left")
        self.direct_pair_hint.pack(anchor="w", pady=8)

    def _build_demo(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=12)
        nb.add(tab, text="Demo")
        ttk.Label(tab, text="Proof-of-work demo: counts the primes below 100,000,000 across all online workers and shows "
                  "which computer did each part. The answer is checked against the known value 5,761,455. "
                  "Use the Benchmark tab for measured comparisons between machines.",
                  wraplength=1000).pack(anchor="w")
        row = ttk.Frame(tab)
        row.pack(anchor="w", pady=8)
        ttk.Label(row, text="Chunks").pack(side="left")
        self.demo_chunks = tk.IntVar(value=40)
        ttk.Spinbox(row, from_=1, to=500, textvariable=self.demo_chunks, width=6).pack(side="left", padx=4)
        ttk.Label(row, text="Only on worker").pack(side="left", padx=(12, 0))
        self.demo_target = ttk.Combobox(row, values=["(all workers)"], state="readonly", width=20)
        self.demo_target.set("(all workers)")
        self.demo_target.pack(side="left", padx=4)
        self.demo_btn = ttk.Button(row, text="Run demo", command=self.run_demo)
        self.demo_btn.pack(side="left", padx=8)
        ttk.Button(row, text="Stop", command=lambda: setattr(self, "_demo_stop", True)).pack(side="left")
        self.demo_out = tk.Text(tab, font=("Consolas", 10))
        self.demo_out.pack(fill="both", expand=True)

    def _build_settings(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=12)
        nb.add(tab, text="Settings & help")
        net = ttk.LabelFrame(tab, text="Network (applies the next time you start the controller)", padding=8)
        net.pack(fill="x")
        ips = list(dict.fromkeys([self.cfg.bind_host, "127.0.0.1"])) if self.cfg.bind_host else ["127.0.0.1"]
        self.bind_var = tk.StringVar(value=self.cfg.bind_host or (ips[0] if ips else "127.0.0.1"))
        self.port_var = tk.IntVar(value=self.cfg.port)
        ttk.Label(net, text="Listen on (this PC's private LAN address)").grid(row=0, column=0, sticky="w")
        bind_choices = ttk.Combobox(net, values=ips, textvariable=self.bind_var, width=42)
        bind_choices.grid(row=0, column=1, sticky="w", padx=6)
        def addresses_ready(values: list[str]) -> None:
            bind_choices.config(values=values)
            if not self.cfg.bind_host and not self.admin and self.bind_var.get() == "127.0.0.1" and values:
                self.bind_var.set(values[0])
        self.bg.run(lambda: list_private_ips(labelled=True) + ["127.0.0.1"], addresses_ready)
        ttk.Label(net, text="Port").grid(row=1, column=0, sticky="w")
        ttk.Spinbox(net, from_=1024, to=65535, textvariable=self.port_var, width=8).grid(row=1, column=1, sticky="w", padx=6)
        ttk.Button(net, text="Save", command=self.save_settings).grid(row=2, column=0, sticky="w", pady=6)
        lw = ttk.LabelFrame(tab, text="This PC as a worker", padding=8)
        lw.pack(fill="x", pady=8)
        limits = default_worker_limits()
        self.local_threads = tk.IntVar(value=limits["max_cpu_threads"])
        self.local_mem = tk.IntVar(value=limits["max_memory_mb"])
        ttk.Label(lw, text="CPU threads to use").grid(row=0, column=0, sticky="w")
        ttk.Spinbox(lw, from_=1, to=128, textvariable=self.local_threads, width=6).grid(row=0, column=1, sticky="w", padx=6)
        ttk.Label(lw, text="Memory per job (MB)").grid(row=1, column=0, sticky="w")
        ttk.Spinbox(lw, from_=256, to=65536, increment=256, textvariable=self.local_mem, width=8).grid(row=1, column=1, sticky="w", padx=6)
        help_text = (
            "Firewall: Windows must allow the i3 to reach this PC on the port above. Run scripts\\add-firewall-rule.ps1 once "
            "as administrator. It only allows your local subnet, on Private networks. Your Wi-Fi must be set to 'Private' "
            "(Settings > Network & internet > Wi-Fi > your network). Never forward this port on your router.\n\n"
            "Secrets (admin token, TLS key, database) are stored in %LOCALAPPDATA%\\NetworkConnector, never in the Git folder.\n"
            "Logs: %LOCALAPPDATA%\\NetworkConnector\\logs. See docs\\TROUBLESHOOTING.md if a worker cannot connect."
        )
        ttk.Label(tab, text=help_text, wraplength=1000, justify="left").pack(anchor="w", pady=8)

    # ------------------------------------------------------------------ controller lifecycle
    def toggle_controller(self) -> None:
        if self.admin:
            if not self.runtime and not messagebox.askyesno(
                    "Stop controller", "Stop the controller started elsewhere? Running jobs will be interrupted."):
                return
            self.start_btn.config(state="disabled")
            self.bg.run(lambda: self._stop_all(shutdown_attached=True), lambda _r: self._set_stopped())
            return
        self.start_btn.config(state="disabled")
        self.status.config(text="Starting …", foreground="#555")
        selected_host = self.bind_var.get().split(" — ", 1)[0].strip()
        selected_port = int(self.port_var.get())

        def work() -> tuple[Any, AdminClient]:
            from .controller import ControllerRuntime

            client = None
            try:
                client = admin_client_from_disk(self.cfg_path.parent)
                client.health()
                client.summary()
                return None, client
            except Exception:
                if client:
                    client.close()
            cfg = load_config(ControllerConfig, self.cfg_path)
            if cfg.bind_host != selected_host:
                cfg.bind_hosts = [h for h in cfg.bind_hosts if h != cfg.bind_host]
            cfg.bind_host, cfg.port = selected_host, selected_port
            rt = ControllerRuntime(cfg)
            try:
                rt.start()
            except Exception:
                rt.stop()
                rt.store.close()
                raise
            return rt, AdminClient("127.0.0.1", rt.cfg.port, rt.cert, rt.admin_token)

        def done(res: tuple[Any, AdminClient]) -> None:
            self.runtime, self.admin = res
            self.cfg = load_config(ControllerConfig, self.cfg_path)
            origin = "" if self.runtime else " (started elsewhere)"
            self.status.config(text=f"Controller running{origin} at {self.cfg.bind_host}:{self.cfg.port}",
                               foreground="#1b5e20")
            self.start_btn.config(text="Stop controller", state="normal")
            self.local_btn.config(state="normal")

        def fail(exc: Exception) -> None:
            self._set_stopped()
            hint = "" if isinstance(exc, ConfigError) else "\n\nIs another copy already running, or is the port in use?"
            messagebox.showerror("Could not start controller", f"{exc}{hint}")

        self.bg.run(work, done, fail)

    def _stop_all(self, shutdown_attached: bool = False) -> None:
        if self.local_agent:
            self.local_agent.stop()
            self.local_agent = None
        if self.admin:
            if shutdown_attached and not self.runtime:
                self.admin.shutdown()
            self.admin.close()
            self.admin = None
        if self.runtime:
            self.runtime.stop()
            self.runtime.store.close()
            self.runtime = None

    def _set_stopped(self) -> None:
        self.status.config(text="Controller stopped", foreground="#b00020")
        self.start_btn.config(text="Start controller", state="normal")
        self.local_btn.config(text="Use this PC as a worker too", state="disabled")
        self.local_status.config(text="")

    def toggle_local_worker(self) -> None:
        from .localworker import ensure_paired, make_agent

        if self.local_agent:
            agent, self.local_agent = self.local_agent, None
            self.local_btn.config(state="disabled")
            self.bg.run(agent.stop, lambda _r: self.local_btn.config(text="Use this PC as a worker too", state="normal"))
            return
        if not self.admin:
            return
        admin, host, port = self.admin, "127.0.0.1", self.cfg.port
        threads, mem = self.local_threads.get(), self.local_mem.get()

        def work() -> Any:
            cfg = ensure_paired(admin, host, port)
            cfg.max_cpu_threads = cfg.max_concurrent_jobs = threads
            cfg.max_memory_mb = mem
            agent = make_agent(cfg)
            agent.start_thread()
            return agent

        def done(agent: Any) -> None:
            self.local_agent = agent
            self.local_btn.config(text="Stop this PC's worker")

        self.bg.run(work, done, lambda e: messagebox.showerror("Local worker", str(e)))

    def on_close(self) -> None:
        if self.runtime and not messagebox.askyesno("Exit", "Stop the controller and exit? Queued jobs are kept and resume next time."):
            return
        self.root.title("Shutting down safely …")
        self.bg.run(self._stop_all, lambda _r: self.root.destroy(), lambda _e: self.root.destroy())

    def save_settings(self, quiet: bool = False) -> None:
        cfg = load_config(ControllerConfig, self.cfg_path)
        previous_host = cfg.bind_host
        cfg.bind_host, cfg.port = self.bind_var.get().split(" — ", 1)[0].strip(), int(self.port_var.get())
        if cfg.bind_host != previous_host:
            cfg.bind_hosts = [h for h in cfg.bind_hosts if h != previous_host]
        try:
            from .config import validate_bind_host

            validate_bind_host(cfg.bind_host)
        except ConfigError as exc:
            messagebox.showerror("Settings", str(exc))
            return
        save_config(cfg, self.cfg_path)
        self.cfg = cfg
        if not quiet:
            messagebox.showinfo("Settings", "Saved. Restart the controller to apply.")

    # ------------------------------------------------------------------ periodic refresh
    def _tick(self) -> None:
        self.root.after(2000, self._tick)
        self._update_pair_countdown()
        if self.local_agent:
            s = self.local_agent.snapshot()
            self.local_status.config(text=f"This PC's worker: {s['state']}, {len(s['running'])} running, {s['completed']} done")
        if not self.admin or self._refreshing:
            return
        self._refreshing = True
        admin, status = self.admin, self.job_filter.get()

        def work() -> tuple[Any, Any, Any]:
            return admin.summary(), admin.workers(), admin.jobs(status=None if status == "all" else status, limit=300)

        def done(res: tuple[Any, Any, Any]) -> None:
            self._refreshing = False
            self._render(*res)

        def fail(_exc: Exception) -> None:
            self._refreshing = False

        self.bg.run(work, done, fail)

    def _render(self, summary: dict[str, Any], workers: list[dict[str, Any]], job_rows: list[dict[str, Any]]) -> None:
        self.tiles["online"].config(text=f"{summary['workers_online']} / {summary['workers_total']}")
        for key in ("queued", "running", "succeeded", "failed", "cancelled"):
            self.tiles[key].config(text=str(summary["jobs"][key]))
        self.workers = {w["id"]: w for w in workers}
        rows = []
        for w in workers:
            m, info, lim = w["metrics"], w["info"], w["info"].get("limits", {})
            hb = "never" if w["seconds_since_heartbeat"] is None else f"{w['seconds_since_heartbeat']:.0f}s ago"
            rows.append((w["id"], [w["name"], w["status"], f"{m.get('cpu_percent', '–')}%", f"{m.get('mem_percent', '–')}%",
                                   info.get("cpu_name", "?"),
                                   f"{lim.get('max_concurrent_jobs', '?')} jobs, {lim.get('max_memory_mb', '?')} MB",
                                   w["running"], w["succeeded"], w["failed"], hb, w["version"], w.get("link", "unknown")]))
        fill_tree(self.wtree, rows)
        live = [w for w in workers if w["history"] and w["status"] != "revoked"]
        self.cpu_graph.draw({w["name"]: [h[1] for h in w["history"]] for w in live})
        self.mem_graph.draw({w["name"]: [h[2] for h in w["history"]] for w in live})
        names = [w["name"] for w in workers if w["status"] != "revoked"]
        self.target.config(values=["(any worker)"] + names)
        self.demo_target.config(values=["(all workers)"] + names)
        fill_tree(self.jtree, [(j["id"], [j["id"], j["batch_id"], j["kind"], j["status"], f"{j['progress']:.0f}%",
                                          j["worker_name"] or "–", j["attempt"],
                                          "–" if j["duration"] is None else f"{j['duration']:.1f}s", (j["error"] or "")[:80]])
                               for j in job_rows])

    # ------------------------------------------------------------------ actions
    def _need_admin(self) -> AdminClient | None:
        if not self.admin:
            messagebox.showinfo("Controller", "Start the controller first.")
        return self.admin

    def worker_action(self, action: str) -> None:
        admin = self._need_admin()
        sel = self.wtree.selection()
        if not admin or not sel:
            return
        name = self.workers[sel[0]]["name"]
        if action == "revoke" and not messagebox.askyesno(
                "Revoke worker", f"Revoke '{name}'? Its token stops working immediately and it must be paired again."):
            return
        self.bg.run(lambda: admin.worker_action(name, action), None, lambda e: messagebox.showerror("Worker", str(e)))

    def _selected_job_id(self) -> str | None:
        sel = self.jtree.selection()
        return sel[0] if sel else None

    def show_job_detail(self) -> None:
        jid, admin = self._selected_job_id(), self.admin
        if not jid or not admin:
            return

        def done(job: dict[str, Any]) -> None:
            self._selected_job = job
            self.job_detail.delete("1.0", "end")
            self.job_detail.insert("1.0", json.dumps(_short(job), indent=2, default=str))

        self.bg.run(lambda: admin.job(jid), done)

    def cancel_selected(self) -> None:
        jid, admin = self._selected_job_id(), self._need_admin()
        if jid and admin:
            self.bg.run(lambda: admin.cancel_job(jid), None, lambda e: messagebox.showerror("Cancel", str(e)))

    def cancel_batch_selected(self) -> None:
        jid, admin = self._selected_job_id(), self._need_admin()
        if jid and admin:
            batch = self.jtree.item(jid)["values"][1]
            if messagebox.askyesno("Cancel batch", f"Cancel every unfinished job in batch {batch}?"):
                self.bg.run(lambda: admin.cancel_batch(str(batch)))

    def retry_selected(self) -> None:
        jid, admin = self._selected_job_id(), self._need_admin()
        if jid and admin:
            self.bg.run(lambda: admin.retry_job(jid), None, lambda e: messagebox.showerror("Retry", str(e)))

    def save_image_result(self) -> None:
        job = self._selected_job
        data = ((job or {}).get("result") or {}).get("output_b64")
        if not data:
            messagebox.showinfo("Save image", "Select a finished image_resize job first (large outputs are not kept inline).")
            return
        path = filedialog.asksaveasfilename(initialfile="resized-" + job["result"]["name"])  # type: ignore[index]
        if path:
            Path(path).write_bytes(base64.b64decode(data))

    def choose_files(self) -> None:
        picked = [Path(p) for p in filedialog.askopenfilenames(title="Choose files to send with the job")]
        too_big = [p.name for p in picked if p.stat().st_size > jobs.MAX_FILE_BYTES]
        if too_big:
            messagebox.showwarning("Files", "Skipped (larger than 8 MB): " + ", ".join(too_big))
        self.files = [p for p in picked if p.stat().st_size <= jobs.MAX_FILE_BYTES][:500]
        self.files_label.config(text=f"{len(self.files)} file(s) chosen" if self.files else "No files chosen")

    def _collect_params(self, kind: str) -> dict[str, Any]:
        raw = {k: v.get() for k, v in self.param_vars.items()}
        if kind in SIMPLE_KINDS:
            return {k: int(str(v).replace(",", "").replace("_", "")) for k, v in raw.items()}
        if kind == "python_function":
            (arg_name, _), = jobs.FUNCTION_ARGS[raw["function"]].items()
            return {"function": raw["function"], "args": {arg_name: int(raw["argument"])}}
        if kind == "hash_files":
            return {"algorithm": raw["algorithm"]}
        return {"width": int(raw["width"]), "height": int(raw["height"]), "mode": raw["mode"], "format": raw["format"]}

    def submit_job(self) -> None:
        admin = self._need_admin()
        if not admin:
            return
        kind = self.kind.get()
        try:
            params = self._collect_params(kind)
        except ValueError:
            messagebox.showerror("Submit", "Parameters must be whole numbers.")
            return
        target = None if self.target.get().startswith("(") else self.target.get()
        opts = {"timeout_s": self.timeout.get(), "max_attempts": self.attempts.get(), "target": target,
                "label": self.label.get() or None, "idempotency_key": self.idem.get().strip() or None}
        if kind in FILE_KINDS:
            if not self.files:
                messagebox.showinfo("Submit", "Choose at least one file.")
                return
            items = []
            for path in self.files:
                data = base64.b64encode(path.read_bytes()).decode()
                p = dict(params)
                if kind == "hash_files":
                    p["files"] = [{"name": path.name, "data_b64": data}]
                else:
                    p.update(name=path.name, image_b64=data)
                items.append({"kind": kind, "params": p})
            work = lambda: admin.submit_batch(items, **opts)  # noqa: E731
        else:
            chunks = self.chunks.get() if jobs.REGISTRY[kind].split else 1
            work = lambda: admin.submit(kind, params, chunks=chunks, **opts)  # noqa: E731

        def done(res: dict[str, Any]) -> None:
            msg = (f"Submitted batch {res['batch_id']} with {res['chunks']} job(s)." if res["created"]
                   else f"Already submitted earlier (batch {res['batch_id']}); duplicate ignored.")
            self.submit_result.config(text=msg)
            self.idem.set(uuid.uuid4().hex[:16])

        self.bg.run(work, done, lambda e: messagebox.showerror("Submit failed", str(e)))

    def create_code(self) -> None:
        admin = self._need_admin()
        if not admin:
            return
        host, port = self.cfg.bind_host, self.cfg.port

        def done(res: dict[str, Any]) -> None:
            self._pair_expires = time.time() + res["expires_in_seconds"]
            self.code_label.config(text=res["code"])
            fp = res["fingerprint"]
            self.code_info.config(text=(
                f"Controller address to type on the worker:   {host}:{port}\n"
                f"Certificate fingerprint (worker shows the start of it):   {fp[:4]} {fp[4:8]} {fp[8:12]} {fp[12:16]}"))
            self.root.clipboard_clear()
            self.root.clipboard_append(res["code"])

        self.bg.run(admin.create_pairing, done, lambda e: messagebox.showerror("Pairing", str(e)))

    def _update_pair_countdown(self) -> None:
        if not self._pair_expires:
            return
        left = int(self._pair_expires - time.time())
        if left <= 0:
            self.code_label.config(text="(expired — create a new code)")
            self._pair_expires = 0
        else:
            self.root.title(f"Network Connector — Controller {__version__}   (pairing code valid {left // 60}:{left % 60:02d})")

    def run_demo(self) -> None:
        from .demo import run_demo

        admin = self._need_admin()
        if not admin:
            return
        self.demo_out.delete("1.0", "end")
        self.demo_btn.config(state="disabled")
        self._demo_stop = False
        target = None if self.demo_target.get().startswith("(") else self.demo_target.get()
        chunks = self.demo_chunks.get()

        def line(text: str) -> None:
            self.demo_out.insert("end", text + "\n")
            self.demo_out.see("end")

        def work() -> Any:
            return run_demo(admin, out=lambda t: self.bg.call_soon(line, t), chunks=chunks, target=target,
                            should_stop=lambda: self._demo_stop)

        def finish(_r: Any) -> None:
            self.demo_btn.config(state="normal")

        def fail(exc: Exception) -> None:
            line(f"\nDemo stopped: {exc}")
            finish(None)

        self.bg.run(work, finish, fail)


def main() -> None:
    setup_logging("controller-gui", console=False)
    root = tk.Tk()
    ControllerApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()
