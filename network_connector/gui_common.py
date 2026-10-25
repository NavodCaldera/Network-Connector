"""Small Tkinter helpers shared by both windows."""
from __future__ import annotations

import queue
import threading
import tkinter as tk
from tkinter import ttk
from typing import Any, Callable

PALETTE = ["#2f6fdf", "#e0622a", "#2a9d5b", "#9b51e0", "#c9a227", "#d13b6b"]


class Background:
    """Run slow work (network calls) off the UI thread and deliver results back on it."""

    def __init__(self, root: tk.Misc) -> None:
        self.root = root
        self._q: queue.Queue[tuple[Callable[..., None], tuple[Any, ...]]] = queue.Queue()
        self._pump()

    def _pump(self) -> None:
        try:
            while True:
                fn, args = self._q.get_nowait()
                fn(*args)
        except queue.Empty:
            pass
        self.root.after(100, self._pump)

    def call_soon(self, fn: Callable[..., None], *args: Any) -> None:
        self._q.put((fn, args))

    def run(self, work: Callable[[], Any], done: Callable[[Any], None] | None = None,
            error: Callable[[Exception], None] | None = None) -> None:
        def target() -> None:
            try:
                result = work()
            except Exception as exc:  # noqa: BLE001 - shown to the user
                if error:
                    self.call_soon(error, exc)
                return
            if done:
                self.call_soon(done, result)

        threading.Thread(target=target, daemon=True).start()


def make_tree(parent: tk.Misc, columns: list[tuple[str, int]], height: int = 8) -> ttk.Treeview:
    frame = ttk.Frame(parent)
    frame.pack(fill="both", expand=True)
    tree = ttk.Treeview(frame, columns=[c for c, _ in columns], show="headings", height=height)
    for name, width in columns:
        tree.heading(name, text=name)
        tree.column(name, width=width, anchor="w")
    bar = ttk.Scrollbar(frame, orient="vertical", command=tree.yview)
    tree.configure(yscrollcommand=bar.set)
    tree.pack(side="left", fill="both", expand=True)
    bar.pack(side="right", fill="y")
    return tree


def fill_tree(tree: ttk.Treeview, rows: list[tuple[str, list[Any]]]) -> None:
    """Replace rows while keeping the user's selection."""
    selected = set(tree.selection())
    tree.delete(*tree.get_children())
    for iid, values in rows:
        tree.insert("", "end", iid=iid, values=values)
    keep = [i for i in selected if tree.exists(i)]
    if keep:
        tree.selection_set(keep)


class Sparkline(tk.Canvas):
    """Live CPU history graph: one line per computer, 0-100%."""

    def __init__(self, parent: tk.Misc, height: int = 140) -> None:
        super().__init__(parent, height=height, background="white", highlightthickness=1, highlightbackground="#ccc")

    def draw(self, series: dict[str, list[float]]) -> None:
        self.delete("all")
        w, h = max(self.winfo_width(), 100), max(self.winfo_height(), 60)
        for pct in (25, 50, 75, 100):
            y = h - 4 - (h - 20) * pct / 100
            self.create_line(30, y, w - 4, y, fill="#eee")
            self.create_text(4, y, text=f"{pct}%", anchor="w", fill="#999", font=("Segoe UI", 7))
        for i, (name, values) in enumerate(series.items()):
            color = PALETTE[i % len(PALETTE)]
            vals = values[-90:]
            if len(vals) >= 2:
                step = (w - 36) / 89
                start = w - 4 - step * (len(vals) - 1)
                pts: list[float] = []
                for k, v in enumerate(vals):
                    pts += [start + k * step, h - 4 - (h - 20) * min(max(v, 0), 100) / 100]
                self.create_line(*pts, fill=color, width=2)
            self.create_text(40 + i * 150, 8, text=f"■ {name}", fill=color, anchor="w", font=("Segoe UI", 8, "bold"))
