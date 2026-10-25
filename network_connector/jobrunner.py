"""Child process that runs ONE allowlisted job.

Protocol: JSON {"kind","params"} on stdin. Progress lines ("PROGRESS 42.0") go to stderr;
the final JSON result goes to stdout. Nothing from stdin is ever executed; it is plain data
handed to jobs.execute(), which only dispatches to the fixed allowlist.
"""
from __future__ import annotations

import json
import sys
import time

from . import jobs


def main() -> int:
    try:
        data = json.loads(sys.stdin.buffer.read())
        kind, params = data["kind"], data["params"]
    except Exception as exc:  # noqa: BLE001
        print(json.dumps({"ok": False, "error": f"bad job input: {exc}", "retryable": False}))
        return 2

    last = [0.0]

    def progress(pct: float) -> None:
        now = time.monotonic()
        if now - last[0] >= 0.5:
            last[0] = now
            print(f"PROGRESS {pct:.1f}", file=sys.stderr, flush=True)

    try:
        out = {"ok": True, "result": jobs.execute(kind, params, progress)}
    except jobs.JobError as exc:
        out = {"ok": False, "error": f"job rejected: {exc}", "retryable": False}
    except MemoryError:
        out = {"ok": False, "error": "job exceeded the worker's memory limit", "retryable": False}
    except Exception as exc:  # noqa: BLE001
        out = {"ok": False, "error": f"{type(exc).__name__}: {exc}", "retryable": True}
    sys.stdout.write(json.dumps(out))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
