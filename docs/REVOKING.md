# Removing or revoking a worker

## Pause, disable or revoke

| Action | Effect | Undo |
|---|---|---|
| **Pause** | The worker finishes its current jobs and takes no new ones | Resume |
| **Disable** | Like pause, but meant as "off until I say so" | Enable |
| **Revoke** | The token is deleted at once. The worker can't connect and stops itself. Its running jobs return to the queue | Pair again with a new code |

**Window (i7):** *Dashboard* → select the worker → **Pause / Resume / Disable / Enable / Revoke (remove)…**

**Command line (i7):**

```powershell
.\.venv\Scripts\nc.exe workers list
.\.venv\Scripts\nc.exe workers revoke i3-laptop
```

Revoke immediately if a worker laptop is lost or stolen.

## Clean up the worker computer

After revoking, on the i3:

1. Close the worker window, or run `nc worker stop`.
2. In the worker window, click **Forget pairing…** (this deletes the token and the pinned certificate), or
   delete the folder `%LOCALAPPDATA%\NetworkConnector\worker`.
3. To uninstall completely, also delete `%LOCALAPPDATA%\NetworkConnector` and the desktop shortcut.

## Remove everything from the i7

1. Stop the controller.
2. As administrator: `powershell -ExecutionPolicy Bypass -File .\scripts\remove-firewall-rule.ps1`
3. Delete `%LOCALAPPDATA%\NetworkConnector` (database, keys, tokens, logs) and the repository folder.
