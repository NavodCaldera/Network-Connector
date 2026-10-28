"""CPU time spent helping each requester, kept locally on the helper."""
from __future__ import annotations

import sqlite3
import time
from datetime import date
from pathlib import Path


REPUTATION_KINDS = ("limit", "oversized", "escape")
OUTCOME_KIND = {"memory": "limit", "timeout": "limit", "stdout_cap": "oversized", "stderr_cap": "oversized",
                "escape": "escape"}


class HelperUsage:
    def __init__(self, path: Path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)
        with sqlite3.connect(path) as db:
            db.execute("CREATE TABLE IF NOT EXISTS helper_usage (day TEXT NOT NULL, requester TEXT NOT NULL, "
                       "cpu_seconds REAL NOT NULL DEFAULT 0, PRIMARY KEY(day, requester))")
            # Per-requester reputation as this helper sees it (PLAN §18): jobs it had to kill.
            db.execute("CREATE TABLE IF NOT EXISTS requester_events (requester TEXT NOT NULL, kind TEXT NOT NULL, "
                       "count INTEGER NOT NULL DEFAULT 0, last REAL, PRIMARY KEY(requester, kind))")

    def seconds(self, requester: str, day: date | None = None) -> float:
        with sqlite3.connect(self.path) as db:
            row = db.execute("SELECT cpu_seconds FROM helper_usage WHERE day=? AND requester=?",
                             ((day or date.today()).isoformat(), requester)).fetchone()
        return float(row[0]) if row else 0.0

    def note(self, requester: str, kind: str) -> None:
        """Count a job of this requester that had to be killed: 'limit', 'oversized' or 'escape'."""
        if not requester or kind not in REPUTATION_KINDS:
            return
        with sqlite3.connect(self.path) as db:
            db.execute("INSERT INTO requester_events(requester, kind, count, last) VALUES(?,?,1,?) "
                       "ON CONFLICT(requester, kind) DO UPDATE SET count=count+1, last=excluded.last",
                       (requester, kind, time.time()))

    def reputation(self, requester: str) -> dict[str, int]:
        with sqlite3.connect(self.path) as db:
            rows = db.execute("SELECT kind, count FROM requester_events WHERE requester=?", (requester,)).fetchall()
        return {kind: int(count) for kind, count in rows}

    def add(self, requester: str, seconds: float, day: date | None = None) -> None:
        if not requester or seconds <= 0:
            return
        with sqlite3.connect(self.path) as db:
            db.execute("INSERT INTO helper_usage(day, requester, cpu_seconds) VALUES(?,?,?) "
                       "ON CONFLICT(day, requester) DO UPDATE SET cpu_seconds=cpu_seconds+excluded.cpu_seconds",
                       ((day or date.today()).isoformat(), requester, seconds))
