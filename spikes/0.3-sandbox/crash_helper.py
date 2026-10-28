"""Throwaway: plays a helper that starts a never-ending sandboxed job and is then killed hard by t6."""
import json
import sys

import ac


def started(r: ac.Run, pid: int) -> None:
    print(json.dumps({"name": r.name, "pid": pid, "folder": r.folder}), flush=True)


ac.run(ac.runner_cmd("loop.wasm", "--fast-platform"), timeout=600, on_start=started)
sys.exit(0)
