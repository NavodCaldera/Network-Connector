# Windows and two-laptop acceptance for 0.3.0

Status: **pending real i7/i3 results**. Tick a step only after observing it. Record
the PC, commit, actual output, elapsed time where relevant, and any failure. A
skipped or blocked step is not a pass. No merge to main or release tag until the
owner accepts these results.

Use public example inputs only. Never include worker.json, controller.json,
tokens, keys, pairing codes, passphrases, or private job data in reports.

## 1. Prepare both PCs before disconnecting the internet

Run these commands in PowerShell on **each PC**, from its repository folder. On
the i3, first clone the repository if it is not already present.

```powershell
git fetch origin
git switch claude/great-davinci-bch39n
git pull --ff-only
& 'C:\Program Files\Python312\python.exe' -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e '.[test,app,wifidirect]'
.\.venv\Scripts\python.exe -c "import network_connector; print(network_connector.__file__)"
git rev-parse HEAD
```

Expected: Python 3.12, version 0.3.0 installed, the printed import path is inside
this checkout, and both PCs have the same commit. Plug both laptops into AC.
Do not stop any existing controller or helper. The isolated acceptance sessions
below use TCP 18765 and UDP 18766, so they do not attach to the normal app.

## 2. Local tests and sandbox checks on each PC

Run on **i7**:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-windows\collect-local.ps1 -PC i7 -FullTests
```

Run on **i3**:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File spikes\0.3-windows\collect-local.ps1 -PC i3 -FullTests
```

The script creates an isolated session under `spikes\0.3-windows\out`, runs the
entire suite without excluding tests, then runs these commands with temporary
`NC_HOME` and `NC_RUNTIME_HOME`:

```powershell
.\.venv\Scripts\nc.exe sandbox setup
.\.venv\Scripts\nc.exe sandbox status --check
.\.venv\Scripts\nc.exe sandbox run examples\wasm\primes\primes.wasm --input-text "0 1000000"
.\.venv\Scripts\nc.exe sandbox run examples\wasm\bench\bench.wasm --input-text "7 3 1"
```

Expected: full pytest summary with no failures/errors; sandbox available and its
self-check successful; primes prints `78498`; benchmark prints one unit with
digest `f21b6b5f35b38489a4d9e714089c3df49b59b43de6c0efb2f9d37b43f43cab6a`.
The escape suite must run on both PCs, including 100 mixed-ending sandbox jobs,
crash recovery, blocked files/network/child processes, and Stop within one second.
Inspect any skips; none of those Windows tests may be skipped for acceptance.

Expected final profile check: no new `NC-Sandbox-*` profiles or folders. The
collector reports the real list; it does not sweep or delete another process's
profiles. If a profile already existed before the test, report it separately.

Send the `*-tests.txt`, `*-sandbox.txt`, and `*-profiles.json` files back. Review
them for unintended private information before sharing. Keep all session data
and runtime copies local; the out folder is ignored by Git.

## 3. Open isolated app sessions

On **each PC**, use a new PowerShell window and load the collector's last session
(the `i7`/`i3` name must match that PC):

```powershell
$pc = 'i3' # use 'i7' on the i7
$s = Get-Content "spikes\0.3-windows\out\$pc-latest-session.json" -Raw | ConvertFrom-Json
$env:NC_HOME = $s.home
$env:NC_RUNTIME_HOME = $s.runtime
$env:NC_PORT = '18765'
$env:NC_CONTROLLER_PORT = '18765'
$env:NC_DISCOVERY_PORT = '18766'
.\.venv\Scripts\network-connector.exe
```

1. **i7 and i3:** first start shows Give performance / Ask for performance / Both.
   Choose **Both** on i7, **Give performance** on i3. Expected: requester/helper
   sections match the choice, one app window per PC, tray icon appears.
2. **Both:** right-click the NC tray icon. Expected: status, Pause, Stop helping,
   Open, Quit. Close the window using X. Expected: process remains in the tray;
   Open restores it. Quit exits only this acceptance app.
3. **Both:** toggle Start at login on, then off. Expected: HKCU Run value
   `NetworkConnector` is created/removed. Record its pre-test state and restore
   it after the check. Do not leave an isolated acceptance app scheduled at login.
   Actual sign-out/login persistence is a separate owner-run check; save other work
   before doing it. Do not sign out merely to complete this checklist.

## 4. Router/LAN discovery, join, consent, trust and block

1. **Both:** connect to the same trusted home LAN. **i7:** Ask for performance,
   Start controller; confirm port 18765 and the i7's own LAN address. Devices:
   Anyone nearby, Announce on. Use an empty or public label.
2. **i7, administrator PowerShell:** allow only the acceptance controller port:
   `powershell -NoProfile -File scripts\add-firewall-rule.ps1 -Port 18765`.
   Record pre-existing rule state before changing an existing named rule; restore
   it afterwards. Never replace an unrelated Python block rule automatically.
3. **i3:** Nearby requesters should list i7 within 5 seconds. If blocked, record
   the failure before using Search. For LAN discovery, the optional narrow rule is:
   `powershell -NoProfile -File scripts\add-firewall-rule.ps1 -Discovery -DiscoveryPort 18766 -Program 'C:\Program Files\Python312\python.exe'`
   (administrator; apply only after checking existing rule state).
4. **i3:** When a requester asks = Ask me each time; Save limits. Select i7,
   Join. Expected: Accept / Decline notification and in-app question; no work
   before Accept. Decline must not join. Repeat without answering for 60 seconds:
   expected Decline, late Accept does nothing.
5. **i3:** select Accept automatically and tell me, Save limits. Rejoin i7.
   Expected: joins without a pairing code, notification contains only requester
   name, short ID and thread count, with Stop. Record whether toast, tray balloon,
   or in-app fallback was used. Test notification buttons while the app runs.
6. **i3:** Verify. **i7:** Devices shows the same six digits. Compare both screens
   before clicking Trust on each. **i7:** Trusted only; expected trusted i3 can
   rejoin, an untrusted acceptance identity cannot. Do not trust mismatched digits.
7. **i7:** set Ask me policy; **i3:** rejoin; **i7:** Accept (then repeat Decline).
   Expected helper waits for requester consent, with no premature work.
8. **i7:** Block i3 during a long job (step 5 below). Expected it stops receiving
   work, its token is revoked, and rejoin fails. Unblock only this test device.
   **i3:** Block this requester separately; expected local stop and no future
   automatic join to this i7 identity until explicitly unblocked.

## 5. Jobs, Stop, owner limits, result checks and benchmarks

All commands below run in the acceptance shell with the environment from step 3.

1. **i7:** upload and run the public primes example, once targeted to the i3's
   worker name shown on the Dashboard, then once to the i7 local helper:

   ```powershell
   .\.venv\Scripts\nc.exe job submit wasm --module examples\wasm\primes\primes.wasm --input-text "0 1000000" --target <worker-name> --wait
   ```

   Expected: each job runs in a fresh sandbox and stdout decodes to `78498`.
   On i7 use Use this PC as a worker too when testing its local helper.
2. **i7:** submit a longer public benchmark module to i3:

   ```powershell
   .\.venv\Scripts\nc.exe job submit wasm --module examples\wasm\bench\bench.wasm --input-text "7 0 100" --seconds 600 --target <i3-worker-name>
   ```

   **i3:** observe What is running (requester ID, label, module SHA-256, CPU/RAM,
   CPU minutes). Click Stop in the toast, then repeat with tray Stop helping.
   Expected: sandbox process gone within 1 s, unfinished job requeued on i7,
   no immediate reclaim, no profile/folder left once cleanup finishes. Record
   timings; visual impression alone cannot prove the one-second threshold.
3. **i3:** set AC-only, unplug AC during another job. Expected: reason shown,
   no new claims; current chunk finishes by default. Repeat with Stop running
   jobs at once selected: expected current work stopped/released. Reconnect AC.
4. **i3:** set allowed hours to `00:00-00:00` (never), then restore blank. Set
   idle-only to 1 minute; interact with keyboard/mouse, then leave idle. Expected:
   no claims until conditions permit. Set daily CPU allowance to 1 minute and run
   enough work to reach it; expected no further claims that day for this requester.
   Restore chosen limits after each check, without editing usage history.
5. **i3:** CPU cap = 2%, jobs = 1; save and restart this helper. Run the long
   module again. Expected Windows Job Object cap present and reduced total CPU
   use in Task Manager (percentage is of all logical CPUs). Set an out-of-range
   value through the configuration test, not the UI: it must be refused rather
   than silently omit enforcement. Restore the original limits.
6. **i7:** run the lying-helper/majority test against isolated test workers:
   `.\.venv\Scripts\python.exe -m pytest -q tests\test_verify.py`.
   Expected liar caught, outvoted and blocked, correct result retained; verifier
   cannot run => failure closed. Do not simulate a liar using a production helper.
7. **i7:** use a closed, pairing-code group for comparisons that include native
   workloads (open groups correctly refuse native jobs). Pair the isolated i3
   helper with the on-screen code; never put the code in a report. Run:

   ```powershell
   .\.venv\Scripts\nc.exe bench run --units 4 --scenarios each,top2,all --engines native,wasm --repeats 1 --wait
   .\.venv\Scripts\nc.exe bench show <benchmark-id>
   .\.venv\Scripts\nc.exe bench export <benchmark-id> --format html -o comparison.html
   ```

   Expected identical answers, device names/short IDs, native/Wasm overhead,
   per-worker shares. **i3:** Performance / `nc worker perf` shows the same result.
   **i7:** HTML opens offline. Repeat Standard, each/all, 3 repeats for meaningful
   speed measurements; no minimum speedup is assumed. Add top3 when a third PC is
   available; report unavailable hardware rather than claiming many-PC validation.
8. **i7:** Cancel all and benchmark Stop during a run. Expected zero queued and
   zero running work once cancellation is acknowledged.
9. **i7:** with the isolated CLI controller running, open `nc gui`, Start controller.
   Expected attaches to that controller. Do not attach to the owner's real home.

## 6. Direct link and hotspot (explicit network changes)

1. **i7:** Wi-Fi Direct tab, blank SSID/passphrase, Start group. Expected open
   `DIRECT-NC-XXXX`, actual direct IP/subnet shown. Record whether home Wi-Fi stays
   connected. If mode A fails, record the real error and try Windows Mobile Hotspot
   manually. No assumption about offline hotspot availability.
2. **i7:** enable Listen on direct link. On administrator PowerShell, use its
   actual address (example only below):

   ```powershell
   powershell -NoProfile -File scripts\add-firewall-rule.ps1 -WiFiDirect -Port 18765 -LocalAddress 192.168.137.1
   ```

   Expected TCP restricted to that local address and its /24 subnet. i3 is never
   added to admin_hosts. Record/restore any previous rule with the same name.
3. **i3:** while still on home Wi-Fi enable Join nearby Wi-Fi Direct groups
   automatically. Expected an offer warning that joining disconnects that Wi-Fi;
   it must not disconnect automatically. Click Join only when ready to test it.
   Repeat with i3 intentionally disconnected: expected auto-join, scans at most
   once per 30 s. Unknown connection state must only offer, never auto-join.
4. **i3:** Search / Find controller. Expected direct gateway found using outbound
   pinned TLS, even if UDP announcements are blocked on the Public link. The
   fingerprint/device ID must match the real i7. Record discovery failures before
   fallback. Add `-Discovery -DiscoveryPort 18766 -Program <base-python.exe>
   -LocalAddress <i3-direct-ip>` only if needed; expected Public rule is scoped to
   that address and /24, UDP only. Never blanket allow Python on Public networks.
5. **i7/i3:** repeat public Wasm primes job over direct link. Expected correct
   answer, Dashboard says Wi-Fi Direct, optional benchmark results are measured.
   Stop/start i7 group: paired i3 re-finds the same certificate without re-pairing.
   A different certificate at the same address must be refused; run
   `pytest -q tests\test_refind.py` for the isolated impostor scenario.
6. **i3:** Leave the group. Expected created Wi-Fi profile removed; record whether
   Windows restores home Wi-Fi. If Wi-Fi is already off, Turn on may be clicked;
   app must never turn a radio off. Do not turn Wi-Fi off just to test this while
   other work depends on the connection.
7. **Both:** repeat discovery/Search on Mobile Hotspot if supported. Record actual
   network category, addressing and outcomes. Tests A-D in
   `spikes/0.3-network/REPORT.md` collect network details; keep SSIDs/MACs local.

## 7. Final checks and cleanup

1. **i3, while helping:** inspect listener ownership:

   ```powershell
   Get-NetTCPConnection -State Listen | Select-Object LocalAddress,LocalPort,OwningProcess
   Get-NetUDPEndpoint -LocalPort 18766 | Select-Object LocalAddress,LocalPort,OwningProcess
   ```

   Match PIDs to this acceptance app. Expected no helper TCP listener, UDP bound
   to specific interfaces only, never `0.0.0.0`/`::`. Unrelated Windows listeners
   are not failures and must not be stopped.
2. **Both:** Quit only the acceptance apps; `nc sandbox status` should report zero
   newly left profiles. Inspect `sandbox.leftovers()` as the collector does. Do
   not sweep/delete a profile belonging to a live owner process.
3. **Both:** restore Start at login and the original named firewall-rule state.
   Remove only rules created for this acceptance run; do not run the blanket
   remove script if those rules existed before testing.
4. **Both:** `nc sandbox setup --remove` in the isolated shell removes only this
   test runtime grant/copy. Keep result files for review. Close the acceptance
   PowerShell windows to discard their environment overrides.
5. **Owner:** report every unchecked/failed item, i3 test summary, public sandbox
   outputs, Stop timing, tray/notification results, direct/hotspot results, profile
   count and commit ID. Hardware acceptance remains pending until these arrive.
