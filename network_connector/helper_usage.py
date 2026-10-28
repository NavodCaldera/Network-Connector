"""CPU time spent helping each requester, kept locally on the helper."""
from __future__ import annotations

import sqlite3
from datetime import date
from pathlib import Path


class HelperUsage:
    def __init__(self, path: Path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)
        with sqlite3.connect(path) as db:
            db.execute("CREATE TABLE IF NOT EXISTS helper_usage (day TEXT NOT NULL, requester TEXT NOT NULL, "
                       "cpu_seconds REAL NOT NULL DEFAULT 0, PRIMARY KEY(day, requester))")

    def seconds(self, requester: str, day: date | None = None) -> float:
        with sqlite3.connect(self.path) as db:
            row = db.execute("SELECT cpu_seconds FROM helper_usage WHERE day=? AND requester=?",
                             ((day or date.today()).isoformat(), requester)).fetchone()
        return float(row[0]) if row else 0.0

    def add(self, requester: str, seconds: float, day: date | None = None) -> None:
        if not requester or seconds <= 0:
            return
        with sqlite3.connect(self.path) as db:
            db.execute("INSERT INTO helper_usage(day, requester, cpu_seconds) VALUES(?,?,?) "
                       "ON CONFLICT(day, requester) DO UPDATE SET cpu_seconds=cpu_seconds+excluded.cpu_seconds",
                       ((day or date.today()).isoformat(), requester, seconds))
