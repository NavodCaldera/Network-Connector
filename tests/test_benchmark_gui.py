"""Check the GUI accepts persisted benchmark result shapes and draws real shares."""
import tkinter as tk
from tkinter import ttk

from network_connector.benchmark_gui import BenchmarkControls


def test_benchmark_controls_render_saved_results():
    root = tk.Tk()
    root.withdraw()
    try:
        controls = BenchmarkControls()
        controls.root = root
        controls.workers = {}
        controls.admin = None
        nb = ttk.Notebook(root)
        controls._build_benchmark(nb)
        controls._bench_render({"id": "saved", "status": "finished", "result": {
            "scenarios": [
                {"scenario": "i7", "wall_s": 10, "min_s": 9, "max_s": 11, "units_per_s": 4},
                {"scenario": "all", "wall_s": 8, "min_s": 8, "max_s": 8, "units_per_s": 5,
                 "per_worker": {"i7": {"units": 30}, "i3": {"units": 10}}},
            ]}})
        assert controls.bench_table.item("i7", "values")[1] == "10.000"
        assert controls.bench_table.item("all", "values")[3] == "5"
        labels = [controls.bench_chart.itemcget(i, "text") for i in controls.bench_chart.find_all()
                  if controls.bench_chart.type(i) == "text"]
        assert "i3: 10 units" in labels
        assert "finished" in controls.bench_status.cget("text")
        controls._bench_render({"id": "live", "status": "running", "result": {
            "progress": {"phase": "running", "scenario": "all", "repeat": 2,
                         "completed_chunks": 3, "total_chunks": 8}}})
        assert "all, repeat 2" in controls.bench_status.cget("text")
        assert controls.bench_progress["value"] == 3
        assert controls.bench_progress["maximum"] == 8
    finally:
        root.destroy()
