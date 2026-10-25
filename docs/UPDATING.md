# Updating

## How releases work

A **release** is a Git tag like `v0.2.0` on your GitHub repository. Workers install **only** tags,
never unfinished work on `main`. Publishing a tag is how you approve a version.

To publish a new version (on the i7):

1. Change `__version__` in `network_connector/__init__.py`, e.g. to `0.2.0`, and note the change in `CHANGELOG.md`.
2. Run the tests: `.\.venv\Scripts\python.exe -m pytest`
3. Commit, tag and push:

```powershell
git add -A
git commit -m "Release 0.2.0"
git tag -a v0.2.0 -m "Network Connector 0.2.0"     # or: git tag -s v0.2.0 (signed)
git push origin main --tags
```

> The tag **must** match `__version__`. The updater checks that the installed code reports the
> tag's version, and rolls back if it doesn't.

## Updating the i3 worker

**Window:** stop the worker, then click **Check for updates** and **Update now**. Close and reopen
the window afterwards.

**Command line:**

```powershell
$nc = "$env:LOCALAPPDATA\NetworkConnector\app\.venv\Scripts\nc.exe"
& $nc version                  # installed version
& $nc worker update --check    # is a newer release available?
& $nc worker stop              # if it is running in another window
& $nc worker update            # install + verify
& $nc worker run
```

What `worker update` does:

1. Refuses to update if the install folder has local edits.
2. `git fetch --tags`, and picks the highest `vX.Y.Z` tag above the installed version.
3. Optionally, with `--verify-signature`, requires a valid signed tag.
4. Checks out the tag, reinstalls dependencies into the same `.venv`, and runs `nc selftest`.
5. Confirms that the self-test reports the new version. **If anything fails, it checks out the
   previous version again and reinstalls it.**
6. Never touches `%LOCALAPPDATA%\NetworkConnector\worker`, so your settings, token and pinned
   certificate are kept.

Re-running `setup-worker.ps1` also updates to the newest tag, and also keeps your settings.

## Updating the i7 controller

```powershell
cd $HOME\Documents\network-connector
git pull
.\.venv\Scripts\python.exe -m pip install -e .
.\.venv\Scripts\nc.exe selftest
```

Restart the controller. Queued jobs and paired workers are kept (they're in `state.db`).

Keep the controller and workers on the same version where possible. Within one minor version
(0.2.x), workers and the controller are compatible.
