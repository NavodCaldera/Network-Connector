"""Worker window for the helper computer (i3): pair, set limits, start/stop/pause, update."""
from __future__ import annotations

import socket
import tkinter as tk
from tkinter import messagebox, ttk
from typing import Any

from . import __version__
from .client import pair_worker
from .config import WorkerConfig, default_worker_limits, load_config, save_config, setup_logging
from .gui_common import Background, fill_tree, make_tree
from .link_gui import WorkerDirectControls, WorkerNearbyControls
from .paths import worker_dir
from .sysinfo import hardware_info, live_metrics, prime_cpu_counter
from .worker import NotPaired, WorkerAgent, load_worker_config


class WorkerApp(WorkerDirectControls, WorkerNearbyControls):
    def __init__(self, root: tk.Tk) -> None:
        self.root = root
        self.bg = Background(root)
        self.dir = worker_dir("worker")
        self.cfg_path = self.dir / "worker.json"
        self.cfg = load_config(WorkerConfig, self.cfg_path)
        if not self.cfg.token:
            for k, v in default_worker_limits().items():
                setattr(self.cfg, k, v)
        self.agent: WorkerAgent | None = None
        self._shown_events = 0
        prime_cpu_counter()
        hw = hardware_info()

        root.title(f"Network Connector — Worker {__version__}")
        root.geometry(f"900x{min(900, max(550, root.winfo_screenheight() - 100))}")
        root.protocol("WM_DELETE_WINDOW", self.on_close)

        # Keep every section reachable on smaller laptop displays.
        canvas = tk.Canvas(root, highlightthickness=0)
        scroll = ttk.Scrollbar(root, orient="vertical", command=canvas.yview)
        canvas.configure(yscrollcommand=scroll.set)
        scroll.pack(side="right", fill="y")
        canvas.pack(side="left", fill="both", expand=True)
        self.body = ttk.Frame(canvas)
        window = canvas.create_window(0, 0, window=self.body, anchor="nw")
        self.body.bind("<Configure>", lambda _e: canvas.configure(scrollregion=canvas.bbox("all")))
        canvas.bind("<Configure>", lambda e: canvas.itemconfigure(window, width=e.width))
        root = self.body

        head = ttk.Frame(root, padding=8)
        head.pack(fill="x")
        ttk.Label(head, text=f"This computer: {hw['hostname']}  ·  {hw['cpu_name']}  ·  {hw['cores_logical']} threads  ·  "
                  f"{hw['ram_total_mb'] / 1024:.1f} GB RAM", font=("Segoe UI", 10, "bold")).pack(anchor="w")
        self.state_label = ttk.Label(head, text="", font=("Segoe UI", 12, "bold"))
        self.state_label.pack(anchor="w", pady=(4, 0))

        self._build_worker_direct()
        self._build_nearby()
        # pairing
        self.pair_box = ttk.LabelFrame(root, text="1. Pair with the controller", padding=8)
        self.pair_box.pack(fill="x", padx=8, pady=4)
        self.addr = tk.StringVar(value=f"{self.cfg.controller_host}:{self.cfg.controller_port}" if self.cfg.controller_host else "")
        self.code = tk.StringVar()
        self.name = tk.StringVar(value=self.cfg.name or (socket.gethostname().lower()[:40] or "i3-worker"))
        for r, (label, var, hint) in enumerate((
                ("Controller address", self.addr, "The IP:port shown on the i7, e.g. 192.168.1.4:8765"),
                ("Pairing code", self.code, "Shown on the i7 under 'Pair a worker'. Works once, for 10 minutes."),
                ("Name for this worker", self.name, "Letters, digits, dot, dash or underscore."))):
            ttk.Label(self.pair_box, text=label).grid(row=r, column=0, sticky="w", pady=2)
            ttk.Entry(self.pair_box, textvariable=var, width=30).grid(row=r, column=1, sticky="w", padx=6)
            ttk.Label(self.pair_box, text=hint, foreground="#666").grid(row=r, column=2, sticky="w")
        btns = ttk.Frame(self.pair_box)
        btns.grid(row=3, column=0, columnspan=3, sticky="w", pady=4)
        self.pair_btn = ttk.Button(btns, text="Pair", command=self.pair)
        self.pair_btn.pack(side="left")
        ttk.Button(btns, text="Forget pairing…", command=self.forget).pack(side="left", padx=6)
        self.pair_status = ttk.Label(self.pair_box, text="")
        self.pair_status.grid(row=4, column=0, columnspan=3, sticky="w")

        # limits
        lim = ttk.LabelFrame(root, text="2. Limits (keep this laptop responsive)", padding=8)
        lim.pack(fill="x", padx=8, pady=4)
        self.v_threads = tk.IntVar(value=self.cfg.max_cpu_threads)
        self.v_jobs = tk.IntVar(value=self.cfg.max_concurrent_jobs)
        self.v_mem = tk.IntVar(value=self.cfg.max_memory_mb)
        self.v_idle = tk.IntVar(value=self.cfg.idle_timeout_minutes)
        self.v_low = tk.BooleanVar(value=self.cfg.low_priority)
        self.v_ac = tk.BooleanVar(value=self.cfg.ac_only)
        self.v_idle_gate = tk.IntVar(value=self.cfg.idle_min)
        self.v_hours = tk.StringVar(value=self.cfg.allowed_hours)
        self.v_daily = tk.IntVar(value=self.cfg.daily_cpu_min)
        items = (("Max CPU threads", self.v_threads, 1, 128, f"This PC has {hw['cores_logical']}. Leave one free to stay smooth."),
                 ("Max jobs at once", self.v_jobs, 1, 64, "Each job uses one thread."),
                 ("Max memory per job (MB)", self.v_mem, 256, 65536, "A job using more is stopped."),
                 ("Stop after idle (minutes)", self.v_idle, 0, 1440, "0 = never."))
        for r, (label, var, lo, hi, hint) in enumerate(items):
            ttk.Label(lim, text=label).grid(row=r, column=0, sticky="w", pady=2)
            ttk.Spinbox(lim, from_=lo, to=hi, textvariable=var, width=8).grid(row=r, column=1, sticky="w", padx=6)
            ttk.Label(lim, text=hint, foreground="#666").grid(row=r, column=2, sticky="w")
        ttk.Checkbutton(lim, text="Run jobs at low priority (recommended: your own apps always come first)",
                        variable=self.v_low).grid(row=4, column=0, columnspan=3, sticky="w")
        ttk.Checkbutton(lim, text="Help only while plugged in", variable=self.v_ac).grid(row=5, column=0, columnspan=3, sticky="w")
        ttk.Label(lim, text="Help only after idle (minutes; 0 = off)").grid(row=6, column=0, sticky="w")
        ttk.Spinbox(lim, from_=0, to=1440, textvariable=self.v_idle_gate, width=8).grid(row=6, column=1, sticky="w")
        ttk.Label(lim, text="Allowed hours (HH:MM-HH:MM; blank = any)").grid(row=7, column=0, sticky="w")
        ttk.Entry(lim, textvariable=self.v_hours, width=16).grid(row=7, column=1, sticky="w")
        ttk.Label(lim, text="Daily CPU minutes per requester (0 = unlimited)").grid(row=8, column=0, sticky="w")
        ttk.Spinbox(lim, from_=0, to=10080, textvariable=self.v_daily, width=8).grid(row=8, column=1, sticky="w")
        ttk.Button(lim, text="Save limits", command=self.save_limits).grid(row=9, column=0, sticky="w", pady=4)

        # controls
        ctl = ttk.LabelFrame(root, text="3. Run", padding=8)
        ctl.pack(fill="x", padx=8, pady=4)
        self.start_btn = ttk.Button(ctl, text="Start worker", command=self.start)
        self.start_btn.pack(side="left")
        self.stop_btn = ttk.Button(ctl, text="Stop", command=self.stop, state="disabled")
        self.stop_btn.pack(side="left", padx=4)
        self.pause_btn = ttk.Button(ctl, text="Pause", command=self.toggle_pause, state="disabled")
        self.pause_btn.pack(side="left", padx=4)
        ttk.Separator(ctl, orient="vertical").pack(side="left", fill="y", padx=10)
        ttk.Button(ctl, text="Check for updates", command=self.check_update).pack(side="left")
        self.update_btn = ttk.Button(ctl, text="Update now", command=self.do_update)
        self.update_btn.pack(side="left", padx=4)
        self.usage = ttk.Label(ctl, text="")
        self.usage.pack(side="right")

        self.jtree = make_tree(root, [("Job", 120), ("Kind", 140), ("Progress", 80), ("Running for", 100)], height=4)
        perf = ttk.LabelFrame(root, text="Performance", padding=6)
        perf.pack(fill="x", padx=8, pady=4)
        self.performance = ttk.Label(perf, text="No shared benchmark loaded.", wraplength=840)
        self.performance.pack(anchor="w")
        self._perf_busy = False
        ttk.Button(perf, text="Refresh comparison", command=self._refresh_performance).pack(anchor="w")
        self.root.after(3000, self._performance_tick)
        ttk.Label(root, text="Activity", font=("Segoe UI", 10, "bold")).pack(anchor="w", padx=8)
        self.log = tk.Text(root, height=10, font=("Consolas", 9))
        self.log.pack(fill="both", expand=True, padx=8, pady=(0, 8))
        self._refresh_paired()
        self.root.after(1000, self._tick)

    def _performance_tick(self) -> None:
        self.root.after(30000, self._performance_tick)
        self._refresh_performance()

    def _refresh_performance(self) -> None:
        if self._perf_busy or not self._paired():
            return
        from .client import latest_benchmark
        self._perf_busy = True
        def work() -> Any:
            return latest_benchmark(load_worker_config(self.dir), self.dir)
        def done(data: Any) -> None:
            self._perf_busy = False
            if not data:
                self.performance.config(text="No finished benchmark involving this computer yet.")
                return
            result = data.get("result") or {}
            text = (f"{data.get('label') or data.get('id')}: combined speedup {result.get('speedup', '?')}x; "
                    f"efficiency {result.get('efficiency', '?')}; identical answers: {result.get('answers_identical', '?')}.")
            cfg = load_worker_config(self.dir)
            for row in result.get("scenarios", []):
                if row.get("scenario") == cfg.name:
                    text += f" This computer's relative speed: {row.get('relative_speed', '?')}x."
                if row.get("scenario") == "all":
                    share = row.get("per_worker", {}).get(cfg.name, {})
                    if share:
                        text += f" Combined work on this computer: {share.get('chunks', '?')} chunks."
            self.performance.config(text=text)
        def fail(exc: Exception) -> None:
            self._perf_busy = False
            self.performance.config(text=f"Comparison unavailable: {exc}")
        self.bg.run(work, done, fail)

    # ------------------------------------------------------------------ helpers
    def _paired(self) -> bool:
        try:
            load_worker_config(self.dir)
            return True
        except NotPaired:
            return False

    def _use_dir(self, directory: Any) -> None:
        """Switch between the paired controller (worker\\) and a joined requester (requesters\\<id>)."""
        self.dir = directory
        self.cfg_path = directory / "worker.json"
        self.cfg = load_config(WorkerConfig, self.cfg_path)
        self._refresh_paired()

    def _refresh_paired(self) -> None:
        if self._paired():
            cfg = load_config(WorkerConfig, self.cfg_path)
            if cfg.group_mode == "open":
                text = f"Joined {cfg.requester_name or cfg.controller_host} as '{cfg.name}' (open group, no code)."
            else:
                text = f"Paired as '{cfg.name}' with {cfg.controller_host}:{cfg.controller_port}."
            self.pair_status.config(text=text, foreground="#1b5e20")
            self.start_btn.config(state="normal" if not self.agent else "disabled")
        else:
            self.pair_status.config(text="Not paired yet.", foreground="#b00020")
            self.start_btn.config(state="disabled")

    def _write(self, text: str) -> None:
        self.log.insert("end", text + "\n")
        self.log.see("end")

    def _apply_limits(self, cfg: WorkerConfig) -> WorkerConfig:
        cfg.max_cpu_threads = max(1, int(self.v_threads.get()))
        cfg.max_concurrent_jobs = max(1, int(self.v_jobs.get()))
        cfg.max_memory_mb = max(256, int(self.v_mem.get()))
        cfg.idle_timeout_minutes = max(0, int(self.v_idle.get()))
        cfg.low_priority = bool(self.v_low.get())
        cfg.ac_only = bool(self.v_ac.get())
        cfg.idle_min = max(0, int(self.v_idle_gate.get()))
        cfg.allowed_hours = self.v_hours.get().strip()
        cfg.daily_cpu_min = max(0, int(self.v_daily.get()))
        return cfg

    # ------------------------------------------------------------------ actions
    def pair(self) -> None:
        host, _, port = self.addr.get().strip().partition(":")
        code, name = self.code.get().strip(), self.name.get().strip()
        if not host or not code or not name:
            messagebox.showinfo("Pair", "Fill in the controller address, the pairing code and a name.")
            return
        cfg = self._apply_limits(load_config(WorkerConfig, self.cfg_path))
        self.pair_btn.config(state="disabled")
        self.pair_status.config(text="Pairing …", foreground="#555")

        def done(new_cfg: WorkerConfig) -> None:
            self.pair_btn.config(state="normal")
            self.code.set("")
            self.cfg = new_cfg
            self._write(f"Paired with {host} as '{name}'. The controller's certificate is now pinned.")
            self._refresh_paired()

        def fail(exc: Exception) -> None:
            self.pair_btn.config(state="normal")
            self._refresh_paired()
            messagebox.showerror("Pairing failed", f"{exc}\n\nCheck the address, that the controller is running, the "
                                 "firewall rule on the i7, and that the code has not expired.")

        self.bg.run(lambda: pair_worker(host, int(port or 8765), code, name, self.dir, cfg), done, fail)

    def forget(self) -> None:
        if self.agent:
            messagebox.showinfo("Forget pairing", "Stop the worker first.")
            return
        if not messagebox.askyesno("Forget pairing", "Remove this computer's token and pinned certificate? "
                                   "You will need a new pairing code to connect again."):
            return
        cfg = load_config(WorkerConfig, self.cfg_path)
        cfg.token = cfg.worker_id = ""
        cfg.controller_fingerprint = ""
        cfg.known_hosts = []
        save_config(cfg, self.cfg_path)
        (self.dir / "controller.crt").unlink(missing_ok=True)
        self._refresh_paired()

    def save_limits(self) -> None:
        cfg = self._apply_limits(load_config(WorkerConfig, self.cfg_path))
        save_config(cfg, self.cfg_path)
        self._write("Limits saved" + (" (they apply the next time you start the worker)." if self.agent else "."))

    def start(self) -> None:
        try:
            cfg = self._apply_limits(load_worker_config(self.dir))
        except NotPaired as exc:
            messagebox.showinfo("Start", str(exc))
            return
        if not messagebox.askyesno("Start worker", f"Start working for {cfg.controller_host}?\n\nUp to "
                                   f"{min(cfg.max_cpu_threads, cfg.max_concurrent_jobs)} job(s) at once, "
                                   f"{cfg.max_memory_mb} MB each. Only built-in, allowlisted job types can run."):
            return
        self._launch(cfg)

    def _launch(self, cfg: WorkerConfig) -> None:
        cfg = self._apply_limits(cfg)
        save_config(cfg, self.cfg_path)
        self.agent = WorkerAgent(cfg, self.dir)
        self._shown_events = 0
        self.agent.start_thread()
        self.start_btn.config(state="disabled")
        self.stop_btn.config(state="normal")
        self.pause_btn.config(state="normal", text="Pause")
        self.update_btn.config(state="disabled")

    def stop(self) -> None:
        agent = self.agent
        if not agent:
            return
        self.stop_btn.config(state="disabled")
        self.pause_btn.config(state="disabled")
        self._write("Stopping helping now; active sandboxes are being killed …")
        agent.stop_helping()
        self.bg.run(agent.stop, lambda _r: self._stopped())

    def _stopped(self) -> None:
        self.agent = None
        self.update_btn.config(state="normal")
        self._refresh_paired()

    def toggle_pause(self) -> None:
        if not self.agent:
            return
        if self.agent.local_paused:
            self.agent.resume()
            self.pause_btn.config(text="Pause")
        else:
            self.agent.pause()
            self.pause_btn.config(text="Resume")

    def check_update(self) -> None:
        from . import updater

        def done(st: Any) -> None:
            msg = (f"Installed: {st.current}\nNewest release: {st.latest or 'none published'}\n\n"
                   + ("An update is available. Stop the worker, then click 'Update now'." if st.update_available
                      else "You are up to date."))
            messagebox.showinfo("Updates", msg)

        self.bg.run(updater.check, done, lambda e: messagebox.showerror("Updates", str(e)))

    def do_update(self) -> None:
        from . import updater

        if self.agent:
            messagebox.showinfo("Update", "Stop the worker first.")
            return
        if not messagebox.askyesno("Update", "Download and install the newest approved release from GitHub?\n"
                                   "Your settings and pairing are kept. If the new version fails its self-test, "
                                   "the old one is restored."):
            return
        self.update_btn.config(state="disabled")

        def done(version: str) -> None:
            self.update_btn.config(state="normal")
            if version != __version__:
                messagebox.showinfo("Update", f"Updated to {version}. Close and reopen this window to use it.")
            else:
                messagebox.showinfo("Update", "Already up to date.")

        def fail(exc: Exception) -> None:
            self.update_btn.config(state="normal")
            messagebox.showerror("Update failed", str(exc))

        self.bg.run(lambda: updater.update(log=lambda t: self.bg.call_soon(self._write, t)), done, fail)

    def on_close(self) -> None:
        self._nearby_stop()
        if self.agent:
            if not messagebox.askyesno("Exit", "Stop the worker and exit? Unfinished jobs go back to the queue."):
                return
            self.root.title("Stopping …")
            self.bg.run(self.agent.stop, lambda _r: self.root.destroy(), lambda _e: self.root.destroy())
        else:
            self.root.destroy()

    # ------------------------------------------------------------------ refresh
    def _tick(self) -> None:
        self.root.after(1000, self._tick)
        m = live_metrics()
        self.usage.config(text=f"This PC now: CPU {m['cpu_percent']}%   RAM {m['mem_percent']}%")
        agent = self.agent
        if not agent:
            self.state_label.config(text="Worker is stopped", foreground="#b00020")
            fill_tree(self.jtree, [])
            return
        s = agent.snapshot()
        if not self._join_busy:
            self.link_status.config(text=f"Link: {s.get('link', 'unknown')} | Controller: "
                                    f"{s.get('controller_host', self.cfg.controller_host)} | RTT: {s.get('rtt_ms', 0):.1f} ms")
        colors = {"online": "#1b5e20", "paused": "#a15c00", "offline": "#b00020", "revoked": "#b00020", "disabled": "#a15c00"}
        text = {"online": "Online — ready for work", "paused": "Paused — no new jobs", "offline": "Offline — retrying connection …",
                "connecting": "Connecting …", "revoked": "Revoked by the controller — pair again",
                "disabled": "Disabled on the controller", "stopping": "Stopping …", "stopped": "Stopped"}.get(s["state"], s["state"])
        self.state_label.config(text=f"{text}    ·    {len(s['running'])} running, {s['completed']} done, {s['failed']} failed",
                                foreground=colors.get(s["state"], "#555"))
        fill_tree(self.jtree, [(j["id"], [j["id"], j["kind"], f"{j['progress']:.0f}%", f"{j['elapsed']:.0f}s"]) for j in s["running"]])
        new = min(agent.event_count - self._shown_events, len(agent.events))
        if new > 0:
            for line in list(agent.events)[-new:]:
                self._write(line)
        self._shown_events = agent.event_count
        if agent._thread and not agent._thread.is_alive() and self.agent is agent:
            self._stopped()
            self.stop_btn.config(state="disabled")
            self.pause_btn.config(state="disabled")


def main() -> None:
    setup_logging("worker-gui", console=False)
    root = tk.Tk()
    WorkerApp(root)
    root.mainloop()


if __name__ == "__main__":
    main()
