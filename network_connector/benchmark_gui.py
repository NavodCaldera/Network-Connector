"""Benchmark controls for the controller window; network work stays off Tk's thread."""
from __future__ import annotations

import json
import tkinter as tk
from tkinter import filedialog, messagebox, ttk
from typing import Any

from .gui_common import PALETTE, fill_tree, make_tree


class BenchmarkControls:
    def _build_benchmark(self, nb: ttk.Notebook) -> None:
        tab = ttk.Frame(nb, padding=8)
        nb.add(tab, text="Benchmark")
        self.bench_id = ""
        self.bench_data: dict[str, Any] = {}
        self._bench_busy = False
        self._bench_history_loaded = False
        inputs = ttk.Frame(tab)
        inputs.pack(fill="x")
        self.bench_workload = tk.StringVar(value="hash_chain")
        self.bench_size = tk.StringVar(value="quick")
        self.bench_repeats = tk.IntVar(value=3)
        self.bench_units = tk.StringVar()
        self.bench_order = tk.BooleanVar(value=True)
        self.bench_wasm = tk.BooleanVar(value=False)
        self.bench_top = tk.StringVar()
        for label, var, values in (("Workload", self.bench_workload, ["hash_chain", "prime_sieve"]),
                                   ("Size", self.bench_size, ["quick", "standard", "long"])):
            ttk.Label(inputs, text=label).pack(side="left", padx=3)
            ttk.Combobox(inputs, textvariable=var, values=values, state="readonly", width=14).pack(side="left")
        ttk.Label(inputs, text="Repeats").pack(side="left", padx=4)
        ttk.Spinbox(inputs, from_=1, to=5, textvariable=self.bench_repeats, width=4).pack(side="left")
        ttk.Label(inputs, text="Units (optional)").pack(side="left", padx=4)
        ttk.Entry(inputs, textvariable=self.bench_units, width=7).pack(side="left")
        ttk.Checkbutton(inputs, text="Also in the Wasm sandbox", variable=self.bench_wasm).pack(side="left", padx=6)
        ttk.Label(inputs, text="Top N").pack(side="left", padx=4)
        ttk.Entry(inputs, textvariable=self.bench_top, width=4).pack(side="left")
        self.bench_workers_box = ttk.Frame(tab)
        self.bench_workers_box.pack(fill="x", pady=5)
        self.bench_workers: dict[str, tk.BooleanVar] = {}
        self.bench_all = tk.BooleanVar(value=True)
        ttk.Checkbutton(self.bench_workers_box, text="All online machines together", variable=self.bench_all).pack(side="left")
        ttk.Label(self.bench_workers_box, text="Solo comparisons:").pack(side="left", padx=4)
        buttons = ttk.Frame(tab)
        buttons.pack(fill="x")
        ttk.Checkbutton(buttons, text="Shuffle scenario order", variable=self.bench_order).pack(side="left")
        for label, fn in (("Run", self._bench_start), ("Stop", self._bench_stop),
                          ("Refresh history", self._bench_history), ("Export", self._bench_export),
                          ("Delete", self._bench_delete)):
            ttk.Button(buttons, text=label, command=fn).pack(side="left", padx=3)
        self.bench_status = ttk.Label(tab, text="Compare identical work on each selected computer and all together.")
        self.bench_status.pack(anchor="w", pady=4)
        self.bench_progress = ttk.Progressbar(tab, mode="indeterminate")
        self.bench_progress.pack(fill="x")
        self.bench_table = make_tree(tab, [("Scenario", 160), ("Wall median (s)", 110), ("Range (s)", 120),
                                          ("Units/s", 90), ("Speedup", 90), ("Wasm overhead", 100)], height=4)
        self.bench_chart = tk.Canvas(tab, height=150, background="white", highlightthickness=0)
        self.bench_chart.pack(fill="x")
        self.bench_detail = tk.Text(tab, height=5, font=("Consolas", 9))
        self.bench_detail.pack(fill="both", expand=True)
        self.bench_history = make_tree(tab, [("ID", 190), ("Label", 190), ("Status", 100)], height=3)
        self.bench_history.bind("<<TreeviewSelect>>", lambda _e: self._bench_open())
        ttk.Label(tab, text="Use AC power and close other CPU-heavy apps. The i7 also runs the controller.").pack(anchor="w")
        self.root.after(2000, self._bench_poll)

    def _bench_sync_workers(self) -> None:
        for worker in self.workers.values():
            name = worker["name"]
            if name not in self.bench_workers:
                var = tk.BooleanVar(value=True)
                self.bench_workers[name] = var
                ttk.Checkbutton(self.bench_workers_box, text=name, variable=var).pack(side="left", padx=4)

    def _bench_error(self, exc: Exception) -> None:
        self._bench_busy = False
        self.bench_status.config(text=str(exc))

    def _bench_start(self) -> None:
        admin = self._need_admin()
        if not admin:
            return
        try:
            spec = {"workload": self.bench_workload.get(), "size": self.bench_size.get(),
                    "scenarios": [name for name, var in self.bench_workers.items() if var.get()]
                                 + (["all"] if self.bench_all.get() else []),
                    "repeats": self.bench_repeats.get(), "order": "shuffled" if self.bench_order.get() else "fixed",
                    "engines": ["native", "wasm"] if self.bench_wasm.get() else ["native"]}
            if self.bench_top.get().strip():
                spec["scenarios"].append(f"top{int(self.bench_top.get())}")
            if self.bench_units.get().strip():
                spec["units"] = int(self.bench_units.get())
                spec.pop("size")
        except (ValueError, tk.TclError) as exc:
            self._bench_error(exc)
            return
        def done(data: dict[str, Any]) -> None:
            self.bench_id = data["id"]
            self._bench_render(data)
        self.bg.run(lambda: admin.start_benchmark(spec), done, self._bench_error)

    def _bench_stop(self) -> None:
        if self.admin and self.bench_id:
            admin, ident = self.admin, self.bench_id
            self.bg.run(lambda: admin.cancel_benchmark(ident), self._bench_render, self._bench_error)

    def _bench_history(self) -> None:
        if not self.admin:
            return
        admin = self.admin
        def done(rows: Any) -> None:
            if isinstance(rows, dict):
                rows = rows.get("benchmarks", [])
            fill_tree(self.bench_history, [(r["id"], [r["id"], r.get("label", ""), r["status"]]) for r in rows])
        self.bg.run(admin.benchmarks, done, self._bench_error)

    def _bench_open(self) -> None:
        selected = self.bench_history.selection()
        if selected:
            self.bench_id = selected[0]

    def _bench_delete(self) -> None:
        if self.admin and self.bench_id and messagebox.askyesno("Delete benchmark", "Delete this saved comparison?"):
            admin, ident = self.admin, self.bench_id
            def done(_r: Any) -> None:
                self.bench_id = ""
                self.bench_data = {}
                self._bench_history()
                self.bench_status.config(text="Comparison deleted.")
            self.bg.run(lambda: admin.delete_benchmark(ident), done, self._bench_error)

    def _bench_poll(self) -> None:
        self.root.after(2000, self._bench_poll)
        self._bench_sync_workers()
        if self.admin and not self._bench_history_loaded:
            self._bench_history_loaded = True
            self._bench_history()
        cpu = ", ".join(f"{w['name']}: {w.get('metrics', {}).get('cpu_percent', '?')}% CPU"
                        for w in self.workers.values())
        if cpu and not self.bench_id:
            self.bench_status.config(text=cpu)
        if self.admin and self.bench_id and not self._bench_busy:
            self._bench_busy = True
            admin, ident = self.admin, self.bench_id
            self.bg.run(lambda: admin.benchmark(ident), self._bench_render, self._bench_error)

    def _bench_render(self, data: dict[str, Any]) -> None:
        self._bench_busy = False
        self.bench_data = data
        status = data.get("status", "unknown")
        runs = data.get("runs") or []
        result = data.get("result") or {}
        progress = result.get("progress") or {}
        current = (f"; {progress.get('phase', '')}: {progress.get('scenario', '')}, repeat {progress.get('repeat', '')}"
                   if progress else (f"; latest scenario: {runs[-1]['scenario']}" if runs else ""))
        cpu = ", ".join(f"{w['name']}: {w.get('metrics', {}).get('cpu_percent', '?')}% CPU"
                        for w in self.workers.values())
        self.bench_status.config(text=f"{data.get('id', self.bench_id)}: {status}{current}  {data.get('error') or ''}  {cpu}")
        self.bench_progress.stop()
        total = progress.get("total_chunks", 0)
        if status == "finished":
            self.bench_progress.config(mode="determinate", maximum=100, value=100)
        elif total:
            self.bench_progress.config(mode="determinate", maximum=total, value=progress.get("completed_chunks", 0))
        elif status in {"queued", "running", "calibrating", "settling"}:
            self.bench_progress.config(mode="indeterminate")
            self.bench_progress.start(15)
        self.bench_detail.delete("1.0", "end")
        self.bench_detail.insert("1.0", json.dumps(result or data, indent=2, default=str))
        self._bench_render_results(result)

    def _bench_render_results(self, result: dict[str, Any]) -> None:
        scenarios = result.get("scenarios", {})
        if isinstance(scenarios, list):
            scenarios = {r.get("scenario", str(i)): r for i, r in enumerate(scenarios)}
        rows = []
        walls = []
        for name, row in scenarios.items():
            wall = row.get("wall_median_s", row.get("wall_s", row.get("median_wall_s", 0)))
            if not isinstance(wall, (float, int)):
                continue
            spread = row.get("range_s", f"{row.get('min_s', wall):.3f} - {row.get('max_s', wall):.3f}")
            rows.append((name, [name, f"{wall:.3f}", str(spread),
                                row.get("throughput", row.get("units_per_s", "")), row.get("speedup", ""),
                                f"{row['wasm_overhead']:.2f}x" if row.get("wasm_overhead") else ""]))
            walls.append((name, wall))
        fill_tree(self.bench_table, rows)
        self.bench_chart.delete("all")
        maximum = max((v for _, v in walls), default=1) or 1
        width = max(200, self.bench_chart.winfo_width()) - 200
        for i, (name, wall) in enumerate(walls):
            y = 10 + i * 25
            self.bench_chart.create_text(5, y + 8, text=name, anchor="w")
            self.bench_chart.create_rectangle(140, y, 140 + width * wall / maximum, y + 17,
                                              fill=PALETTE[i % len(PALETTE)], outline="")
        combined = scenarios.get("all", {}).get("per_worker", {})
        if combined:
            y = 15 + len(walls) * 25
            self.bench_chart.config(height=max(150, y + 50))
            self.bench_chart.create_text(5, y + 8, text="Combined work", anchor="w")
            total = sum(w.get("units", 0) for w in combined.values()) or 1
            x = 140.0
            for i, (name, worker) in enumerate(combined.items()):
                end = x + width * worker.get("units", 0) / total
                self.bench_chart.create_rectangle(x, y, end, y + 18, fill=PALETTE[i % len(PALETTE)], outline="")
                self.bench_chart.create_text(x, y + 32, text=f"{name}: {worker.get('units', 0)} units", anchor="w")
                x = end

    def _bench_export(self) -> None:
        if not self.bench_data:
            return
        path = filedialog.asksaveasfilename(defaultextension=".html", filetypes=[("HTML", "*.html"),
                                           ("JSON", "*.json"), ("CSV", "*.csv")])
        if not path:
            return
        from pathlib import Path
        from .benchmark import export_benchmark
        data = self.bench_data
        self.bg.run(lambda: Path(path).write_text(export_benchmark(data, Path(path).suffix.lstrip(".")), encoding="utf-8"),
                    lambda _r: self.bench_status.config(text=f"Exported to {path}"), self._bench_error)

    def cancel_all_jobs(self) -> None:
        admin = self._need_admin()
        if admin and messagebox.askyesno("Cancel all jobs", "Cancel every queued and running job?"):
            self.bg.run(admin.cancel_all, lambda r: messagebox.showinfo("Cancel all", str(r)),
                        lambda e: messagebox.showerror("Cancel all", str(e)))
